// 录音模块：第 1 讲的全部内容
// 铁律：音频回调里只做"把样本倒进桶"（外加一个廉价的音量统计），
// 打包 WAV、广播事件这些活都留给停止之后的普通线程。
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use serde::{Deserialize, Serialize};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

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
    // 开流计时（毫秒，0 = 尚未发生）：区分"设备预热"与"用户开口反应"的观测点——
    // 首回调（设备开始送数据，哪怕全零）/ 首非零（麦克真的听到了什么）
    pub first_cb_ms: Arc<AtomicU32>,
    pub first_nonzero_ms: Arc<AtomicU32>,
    native_rate: u32,
}

// cpal 的 Stream 没标 Send（内部有裸指针），无法放进全局状态。
// macOS CoreAudio 的流句柄跨线程移动实际是安全的——这里显式担保。
// （这是 cpal 社区的通行做法，见 cpal issue #81）
#[allow(dead_code)] // 持有它本身就是目的（Drop 时停止录音）
struct SendStream(cpal::Stream);
unsafe impl Send for SendStream {}

// ---------- 开流配置缓存 ----------
// 每次开流最贵的不是建流，是 supported_input_configs 的全量能力枚举（设备 IO）。
// 缓存"上次成功"的 (设备名, 采样率, 声道)，命中即跳过枚举直接建流；失配/建流失败
// 自动回退全量枚举并更新缓存——听写永远不因缓存而不可用。持久化在 mic-cache.json，
// 重启后的首次开流同样吃缓存。

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MicCache {
    pub device: String,
    pub rate: u32,
    pub channels: u16,
}

fn cache_load(path: Option<&Path>) -> Option<MicCache> {
    serde_json::from_str(&std::fs::read_to_string(path?).ok()?).ok()
}

fn cache_save(path: Option<&Path>, cache: &MicCache) {
    let Some(p) = path else { return };
    if let Ok(json) = serde_json::to_string(cache) {
        if let Ok(mut f) = std::fs::File::create(p) {
            let _ = f.write_all(json.as_bytes());
        }
    }
}

// 分段计时落盘（开流各段耗时是"唤醒中还有多长"的决策数据，与 /tmp/sayit-focus.log 同款取证）。
// pub：commands.rs 的观察线程也要把"首真信号"（设备预热段）写进来——那是保温决策的关键数
pub fn mic_log(line: &str) {
    println!("[mic] {line}");
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open("/tmp/sayit-mic.log")
    {
        let _ = writeln!(f, "{line}");
    }
}

/// 全量能力枚举：单声道优先、采样率尽量靠近 16k（缓存未命中/失效时的完整路径）
fn pick_config(device: &cpal::Device) -> Result<cpal::StreamConfig, String> {
    let mut best: Option<(cpal::StreamConfig, i64)> = None;
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
            best = Some((
                cpal::StreamConfig {
                    channels: ch,
                    sample_rate: cpal::SampleRate(want),
                    buffer_size: cpal::BufferSize::Default,
                },
                score,
            ));
        }
    }
    best.map(|(c, _)| c)
        .ok_or_else(|| "麦克风不支持可用的录音格式（需要 F32 输入）".into())
}

pub fn start() -> Result<RecordingHandle, String> {
    start_with_cache(None)
}

pub fn start_with_cache(cache_path: Option<&Path>) -> Result<RecordingHandle, String> {
    let t_total = Instant::now();

    // ① 默认设备
    let t = Instant::now();
    let device = cpal::default_host()
        .default_input_device()
        .ok_or("找不到麦克风设备（检查是否连接/授权）")?;
    let device_name = device.name().unwrap_or_default();
    let ms_dev = t.elapsed().as_millis();

    // ② 选配置：缓存命中 → 直接建（失败回退枚举）；未命中 → 全量枚举
    let cached = cache_load(cache_path).filter(|c| c.device == device_name);
    let (config, config_src) = if let Some(c) = cached {
        let cfg = cpal::StreamConfig {
            channels: c.channels,
            sample_rate: cpal::SampleRate(c.rate),
            buffer_size: cpal::BufferSize::Default,
        };
        (cfg, "缓存命中")
    } else {
        (pick_config(&device)?, "全量枚举")
    };
    let ms_config = t.elapsed().as_millis() - ms_dev;

    let channels = config.channels as usize;
    let native_rate = config.sample_rate.0;
    let buf = Arc::new(Mutex::new(SharedBuf {
        samples: Vec::new(),
        rms_idx: 0,
    }));
    let level = Arc::new(AtomicU32::new(0));
    let live = Arc::new(AtomicBool::new(false));
    let first_cb_ms = Arc::new(AtomicU32::new(0));
    let first_nonzero_ms = Arc::new(AtomicU32::new(0));
    let opened_at = Instant::now();

    // ③ 建流（缓存配置被设备拒绝 → 回退全量枚举重试一次，缓存跟着换血）
    let build = |cfg: &cpal::StreamConfig| -> Result<cpal::Stream, String> {
        device
            .build_input_stream(
                cfg,
                {
                    let buf = buf.clone();
                    let level = level.clone();
                    let live = live.clone();
                    let first_cb_ms = first_cb_ms.clone();
                    let first_nonzero_ms = first_nonzero_ms.clone();
                    move |data: &[f32], _| {
                        // 观测点：首回调 / 首非零（区分设备预热与用户开口反应，见 RecordingHandle 注释）
                        // max(1)：0 保留为"从未"哨兵——首回调若落在第 0 毫秒且带非零样本，
                        // 存 0 会让死麦判定（MicDead 恒零检测）把健康麦误判成死麦（③审查 P1）
                        let now_ms = (opened_at.elapsed().as_millis() as u32).max(1);
                        if first_cb_ms.load(Ordering::Relaxed) == 0 {
                            first_cb_ms.store(now_ms, Ordering::Relaxed);
                        }
                        if first_nonzero_ms.load(Ordering::Relaxed) == 0
                            && data.iter().any(|s| s.abs() > 0.0005)
                        {
                            first_nonzero_ms.store(now_ms, Ordering::Relaxed);
                        }
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
                            // 是首个超过开录判定线（RMS>0.001；与死麦线 peak<0.0001 是两条不同的线）的真实信号，加载态才熬得够久
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
            .map_err(|e| format!("无法打开麦克风：{e}（若刚弹出授权窗，请允许后重试）"))
    };

    let (stream, config, config_src) = match build(&config) {
        Ok(s) => (s, config, config_src),
        Err(first_err) => {
            // 缓存配置被拒（设备换了内部格式等）：全量枚举再来一次
            let config = pick_config(&device).map_err(|_| first_err)?;
            let s = build(&config)?;
            (s, config, "缓存失效·回退枚举")
        }
    };

    stream
        .play()
        .map_err(|e| format!("无法启动录音：{e}"))?;

    // ④ 缓存换血 + 分段计时落盘（"唤醒中"优化的决策数据）
    cache_save(
        cache_path,
        &MicCache {
            device: device_name,
            rate: config.sample_rate.0,
            channels: config.channels,
        },
    );
    mic_log(&format!(
        "开流完成：设备 {ms_dev}ms + 配置({config_src}) {ms_config}ms + 建流启动 {}ms = 总 {}ms（{}Hz/{}ch）",
        t_total.elapsed().as_millis() - ms_dev - ms_config,
        t_total.elapsed().as_millis(),
        config.sample_rate.0,
        config.channels,
    ));

    Ok(RecordingHandle {
        stream: SendStream(stream),
        buf,
        level,
        live,
        first_cb_ms,
        first_nonzero_ms,
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
        let pk = peak(&native);

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
        write_pcm_wav(&path, &pcm)?;

        Ok((path, secs, count, pk))
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
    Ok(SelftestReport {
        samples: samples.len(),
        peak: peak(&samples),
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

// 整段峰值（死麦判定：全零假数据的铁证）
fn peak(samples: &[f32]) -> f32 {
    samples.iter().fold(0.0f32, |m, s| m.max(s.abs()))
}

// 打包 16kHz/16bit 单声道 WAV（识别链路的输入格式）
fn write_pcm_wav(path: &Path, pcm: &[i16]) -> Result<(), String> {
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: TARGET_RATE,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut w = hound::WavWriter::create(path, spec).map_err(|e| e.to_string())?;
    for s in pcm {
        w.write_sample(*s).map_err(|e| e.to_string())?;
    }
    w.finalize().map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 配置缓存_落盘回读一致_损坏文件安全回退() {
        let path = std::env::temp_dir().join(format!("sayit-mic-cache-test-{}.json", std::process::id()));
        let c = MicCache { device: "EDIFIER".into(), rate: 48_000, channels: 1 };
        cache_save(Some(&path), &c);
        assert_eq!(cache_load(Some(&path)), Some(c));
        // 损坏内容 → None（安全侧：当作无缓存，回退全量枚举）
        std::fs::write(&path, "garbage").unwrap();
        assert_eq!(cache_load(Some(&path)), None);
        // 路径不存在 → None
        let _ = std::fs::remove_file(&path);
        assert_eq!(cache_load(Some(&path)), None);
        // 无路径（selftest 等无缓存调用方）→ None 且不 panic
        assert_eq!(cache_load(None), None);
    }

    #[test]
    fn 重采样_48k降16k_长度与幅度保持() {
        let flat = vec![0.5f32; 480];
        let out = resample(&flat, 48_000, 16_000);
        assert_eq!(out.len(), 160);
        assert!(out.iter().all(|s| (*s - 0.5).abs() < 1e-6), "常数序列应保幅");
        // 48 个样本（48k 下的一个整周期）应精确落为 16 个
        let sine: Vec<f32> = (0..48).map(|i| (i as f32 / 48.0 * std::f32::consts::TAU).sin()).collect();
        assert_eq!(resample(&sine, 48_000, 16_000).len(), 16);
    }

    #[test]
    fn 重采样_同率直通_空输入为空() {
        let x = vec![0.1f32, 0.2, 0.3];
        assert_eq!(resample(&x, 16_000, 16_000), x);
        assert!(resample(&[], 48_000, 16_000).is_empty());
    }

    #[test]
    fn 峰值_全零为零_非零取最大绝对值() {
        assert_eq!(peak(&[0.0; 100]), 0.0);
        assert_eq!(peak(&[0.1, -0.4, 0.2]), 0.4);
    }

    #[test]
    fn wav打包回读一致() {
        let path = std::env::temp_dir().join(format!("sayit-wav-test-{}.wav", std::process::id()));
        let pcm: Vec<i16> = vec![0, 100, -200, 32767, -32768];
        write_pcm_wav(&path, &pcm).unwrap();
        let mut r = hound::WavReader::open(&path).unwrap();
        assert_eq!((r.spec().sample_rate, r.spec().channels), (TARGET_RATE, 1));
        let back: Vec<i16> = r.samples().map(|s| s.unwrap()).collect();
        assert_eq!(back, pcm);
        let _ = std::fs::remove_file(&path);
    }
}
