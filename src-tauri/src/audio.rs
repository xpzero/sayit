// 录音模块：第 1 讲的全部内容
// 铁律：音频回调里只做"把样本倒进桶"（外加一个廉价的音量统计），
// 打包 WAV、广播事件这些活都留给停止之后的普通线程。
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

const TARGET_RATE: u32 = 16_000; // 语音识别的标准采样率

// 一次进行中的录音（"桶"）
struct SharedBuf {
    samples: Vec<f32>, // 单声道、设备原生采样率
    rms_idx: usize,    // 上次算音量统计到哪了
}

pub struct RecordingHandle {
    stream: SendStream,
    buf: Arc<Mutex<SharedBuf>>,
    pub level: Arc<AtomicU32>, // RMS * 1000，广播线程每 50ms 读一次
    pub live: Arc<AtomicBool>, // 已收到首个超过死麦线的真实信号 = 采集链路真的活了
    native_rate: u32,
}

// cpal 的 Stream 没标 Send（内部有裸指针），无法放进全局状态。
// macOS CoreAudio 的流句柄跨线程移动实际是安全的——这里显式担保。
// （这是 cpal 社区的通行做法，见 cpal issue #81）
#[allow(dead_code)] // 持有它本身就是目的（Drop 时停止录音）
struct SendStream(cpal::Stream);
unsafe impl Send for SendStream {}

pub fn start() -> Result<RecordingHandle, String> {
    let device = cpal::default_host()
        .default_input_device()
        .ok_or("找不到麦克风设备（检查是否连接/授权）")?;

    // 在设备支持的配置里挑一个最接近目标的：单声道优先、采样率尽量靠近 16k
    let mut best: Option<(cpal::SupportedStreamConfig, i64)> = None;
    for range in device
        .supported_input_configs()
        .map_err(|e| format!("无法读取麦克风能力：{e}"))?
    {
        if range.sample_format() != cpal::SampleFormat::F32 {
            continue;
        }
        let ch = range.channels();
        if ch > 2 {
            continue;
        }
        let min = range.min_sample_rate().0;
        let max = range.max_sample_rate().0;
        let want = TARGET_RATE.clamp(min, max);
        let score = (want as i64 - TARGET_RATE as i64).abs() + (ch as i64 - 1) * 10_000;
        let is_better = best
            .as_ref()
            .map(|(_, prev_score)| score < *prev_score)
            .unwrap_or(true);
        if is_better {
            best = Some((range.with_sample_rate(cpal::SampleRate(want)), score));
        }
    }
    let (config, _) = best.ok_or("麦克风不支持可用的录音格式（需要 F32 输入）")?;

    let channels = config.channels() as usize;
    let native_rate = config.sample_rate().0;
    let buf = Arc::new(Mutex::new(SharedBuf {
        samples: Vec::new(),
        rms_idx: 0,
    }));
    let level = Arc::new(AtomicU32::new(0));
    let live = Arc::new(AtomicBool::new(false));

    let stream = device
        .build_input_stream(
            &config.into(),
            {
                let buf = buf.clone();
                let level = level.clone();
                let live = live.clone();
                move |data: &[f32], _| {
                    let mut b = match buf.lock() {
                        Ok(b) => b,
                        Err(_) => return,
                    };
                    // 立体声降混为单声道（左右取平均）
                    if channels == 2 {
                        b.samples.extend(data.chunks(2).map(|c| {
                            if c.len() == 2 {
                                (c[0] + c[1]) / 2.0
                            } else {
                                c[0]
                            }
                        }));
                    } else {
                        b.samples.extend_from_slice(data);
                    }
                    // 每攒够约 50ms 算一次音量（平方和开方 = RMS）
                    let need = (native_rate / 20) as usize;
                    if b.samples.len() - b.rms_idx >= need {
                        let chunk = &b.samples[b.rms_idx..];
                        let sum: f32 = chunk.iter().map(|s| s * s).sum();
                        let rms = (sum / chunk.len() as f32).sqrt();
                        level.store((rms * 1000.0) as u32, Ordering::Relaxed);
                        // 设备预热会送一段"零数据"（play 成功后 ~0.5s，回调在跑但内容是零，
                        // 实测本机 USB 耳机麦）——此刻说的话不会被采样。"开录"信号必须
                        // 是首个超过死麦线（与死麦检测同一条线）的真实信号，加载态才熬得够久
                        if !live.load(Ordering::Relaxed) && rms > 0.001 {
                            live.store(true, Ordering::Relaxed);
                        }
                        b.rms_idx = b.samples.len();
                    }
                }
            },
            |err| eprintln!("[audio] 录音流错误：{err}"),
            Some(Duration::from_secs(1)),
        )
        .map_err(|e| format!("无法打开麦克风：{e}（若刚弹出授权窗，请允许后重试）"))?;

    stream
        .play()
        .map_err(|e| format!("无法启动录音：{e}"))?;

    Ok(RecordingHandle {
        stream: SendStream(stream),
        buf,
        level,
        live,
        native_rate,
    })
}

impl RecordingHandle {
    // 内部工具：停流并取走原始样本（单声道、原生采样率）
    fn into_samples(self) -> Result<(Vec<f32>, u32), String> {
        // 先丢掉 stream：Drop 即停止回调，保证不再有新样本进来
        drop(self.stream);
        let native = std::mem::take(&mut self.buf.lock().map_err(|e| e.to_string())?.samples);
        Ok((native, self.native_rate))
    }

    /// 停止录音并打包成 16kHz/16bit 单声道 WAV，返回 (文件路径, 时长秒, 采样数, 峰值)
    pub fn stop(self, out_dir: &PathBuf) -> Result<(PathBuf, f32, usize, f32), String> {
        let (native, native_rate) = self.into_samples()?;
        let count = native.len();
        // 时长（秒），随路径一起返回给调用方
        let secs = count as f32 / native_rate as f32;
        // 整段峰值：一个非零样本都没有 = 全零假数据（权限被拒的麦只会送这个）——
        // 死麦判定放到停止时刻做，录音期间不打扰用户
        let peak = native.iter().fold(0.0f32, |m, s| m.max(s.abs()));

        // 原生采样率 -> 16k 线性重采样
        let resampled = resample(&native, native_rate, TARGET_RATE);

        // f32 -> i16
        let pcm: Vec<i16> = resampled
            .iter()
            .map(|s| (s.clamp(-1.0, 1.0) * i16::MAX as f32) as i16)
            .collect();

        std::fs::create_dir_all(out_dir).map_err(|e| e.to_string())?;
        let path = out_dir.join(format!(
            "recording-{}.wav",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis()
        ));

        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: TARGET_RATE,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let mut w = hound::WavWriter::create(&path, spec).map_err(|e| e.to_string())?;
        for s in pcm {
            w.write_sample(s).map_err(|e| e.to_string())?;
        }
        w.finalize().map_err(|e| e.to_string())?;

        Ok((path, secs, count, peak))
    }
}

// 麦克风自检报告
pub struct SelftestReport {
    pub samples: usize, // 收到多少样本（0 = 设备根本没送数据）
    pub peak: f32,      // 峰值振幅（接近 0 = 全零静音，多半是权限被拒）
}

// 录制 ms 毫秒并分析：不落盘、只出报告
pub fn selftest(ms: u64) -> Result<SelftestReport, String> {
    let handle = start()?;
    std::thread::sleep(Duration::from_millis(ms));
    let (samples, _) = handle.into_samples()?;
    let peak = samples.iter().fold(0.0f32, |m, s| m.max(s.abs()));
    Ok(SelftestReport {
        samples: samples.len(),
        peak,
    })
}

// 线性插值重采样：听着不 hi-fi，但识别模型毫不在意
fn resample(input: &[f32], from: u32, to: u32) -> Vec<f32> {
    if from == to || input.is_empty() {
        return input.to_vec();
    }
    let ratio = to as f64 / from as f64;
    let out_len = ((input.len() as f64) * ratio).floor() as usize;
    let mut out = Vec::with_capacity(out_len);
    for i in 0..out_len {
        let pos = i as f64 / ratio;
        let j = pos.floor() as usize;
        let frac = (pos - j as f64) as f32;
        let l = input[j];
        let r = input[(j + 1).min(input.len() - 1)];
        out.push(l + (r - l) * frac);
    }
    out
}
