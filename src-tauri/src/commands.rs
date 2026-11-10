// 编排层：用例流程都在这里。快捷键按下 -> toggle_recording_impl 是全应用的心跳。
use crate::audio;
use crate::config::{self, Config};
use crate::engines;
use crate::hotkey;
use std::sync::atomic::Ordering;
use std::time::Duration;
use crate::panel;
use crate::system;
use tauri::{AppHandle, Emitter, Manager, State};

#[tauri::command]
pub fn get_config(app: AppHandle) -> Config {
    config::load(&app)
}

#[tauri::command]
pub fn save_config(app: AppHandle, config: Config) -> Result<(), String> {
    let old = config::load(&app);
    config::save(&app, &config)?;
    // "保存即应用"：增量同步快捷键（无变化时是空操作，可放心每次调用）
    if let Err(e) = hotkey::register_all(&app) {
        return Err(format!("已保存，但部分快捷键未生效：{e}"));
    }
    // 识别方向或本地模型变了 -> 常驻引擎需要换血（下次识别时自动重拉）
    if old.stt.provider != config.stt.provider
        || old.stt.local_model_id != config.stt.local_model_id
    {
        crate::stt::local::kill_engine(&app);
    }
    Ok(())
}

#[tauri::command]
pub fn detect_engine(app: AppHandle) -> engines::EngineStatus {
    let cfg = config::load(&app);
    engines::detect(&app, cfg.engine.path)
}

// ---------- 听写编排：状态机（dictation.rs）+ 效果执行 ----------
// 铁律：相位迁移只经 dictation::step；这里只喂数据、执行产出的效果（IO/线程/面板全是壳的活）。

// tap 回调要无锁判断"Esc 是否有意义"（回调等互斥锁超时会被系统禁用 tap），维护相位镜像
pub(crate) static ACTIVE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

// 供快捷键 handler 调用（无 State 参数版本的入口）
pub fn toggle_recording_impl(app: &AppHandle) {
    toggle_core(app);
}

#[tauri::command]
pub fn toggle_recording(app: AppHandle) {
    toggle_core(&app);
}

fn toggle_core(app: &AppHandle) {
    let cfg = config::load(app);
    // 前置检查：只在即将开新篇时（空闲/识别中）。本地识别却没有就绪模型 → 不录，直接带路。
    // 注意这一步不进状态机——它是配置问题，不是生命周期问题
    {
        let state = app.state::<crate::state::SharedState>();
        let Ok(s) = state.lock() else { return };
        if matches!(
            s.flow.phase,
            crate::dictation::Phase::Idle | crate::dictation::Phase::Transcribing
        ) && cfg.stt.provider != "openai_compatible"
        {
            let ready = crate::models::library(app).into_iter().any(|m| {
                m.id == cfg.stt.local_model_id.clone().unwrap_or_default() && m.status == "ready"
            });
            if !ready {
                drop(s);
                panel::show_panel(app);
                let _ = app.emit(
                    "result",
                    serde_json::json!({ "error": "还没有可用的识别模型", "reason": "no_model" }),
                );
                return;
            }
        }
    }
    send_event(app, crate::dictation::Event::TogglePressed, None);
}

// 录音中途放弃（Esc）：真正的清理由状态机产出的效果驱动
pub fn abort_recording_impl(app: &AppHandle) {
    if !ACTIVE.load(Ordering::Relaxed) {
        return; // 开麦/录音之外 Esc 无意义（别为每次按键空跑主线程）
    }
    send_event(app, crate::dictation::Event::Abort, None);
}

/// 事件入口：锁内走一步迁移，并把 MicReady 的流与迁移在**同一锁区**装好——
/// 绝不让"已 Recording 却无流"或"已装流却已 Idle"的中间态暴露给并发事件
/// （独立测试员抓出的 P1：迁移与装流若分两次拿锁，Esc/停止落进缝隙会产生
/// 孤儿流（level 永久广播、样本无限涨）或把相位冻在 Transcribing）。
/// 效果的执行仍在锁外。
fn send_event(
    app: &AppHandle,
    ev: crate::dictation::Event,
    mut stream: Option<crate::audio::RecordingHandle>,
) {
    let state = app.state::<crate::state::SharedState>();
    let (effects, watch) = {
        let Ok(mut s) = state.lock() else {
            eprintln!("[flow] 状态锁不可用，事件被丢弃：{ev:?}");
            return;
        };
        let (next, effects) = crate::dictation::step(s.flow, &ev);
        s.flow = next;
        ACTIVE.store(next.abortable(), Ordering::Relaxed); // 锁内存镜像，与相位同拍
        let mut watch = None;
        if matches!(effects.first(), Some(crate::dictation::Effect::InstallAndWatch)) {
            if let Some(h) = stream.take() {
                let live = h.live.clone();
                let level = h.level.clone();
                let first_cb = h.first_cb_ms.clone();
                let first_nonzero = h.first_nonzero_ms.clone();
                s.level = Some(level.clone());
                s.broadcasting.store(true, Ordering::Relaxed);
                let flag = s.broadcasting.clone();
                s.recording = Some(h);
                watch = Some((live, level, flag, first_cb, first_nonzero));
            }
        }
        (effects, watch)
    };
    let cfg = config::load(app);
    run_effects(app, &state, &cfg, effects, watch);
    if stream.is_some() {
        println!("[toggle] 丢弃无主流（开麦完成时状态已翻篇，时序竞态）");
    }
}

/// 观察五元组：(live 真信号, level 音量, broadcasting 开关, 首回调 ms, 首非零 ms)
type Watch = Option<(
    std::sync::Arc<std::sync::atomic::AtomicBool>,
    std::sync::Arc<std::sync::atomic::AtomicU32>,
    std::sync::Arc<std::sync::atomic::AtomicBool>,
    std::sync::Arc<std::sync::atomic::AtomicU32>,
    std::sync::Arc<std::sync::atomic::AtomicU32>,
)>;

fn run_effects(
    app: &AppHandle,
    state: &State<'_, crate::state::SharedState>,
    cfg: &config::Config,
    effects: Vec<crate::dictation::Effect>,
    mut watch: Watch,
) {
    use crate::dictation::Effect;
    for eff in effects {
        match eff {
            Effect::Ignored => {}
            Effect::SpawnOpener => spawn_opener(app, cfg),
            Effect::InstallAndWatch => {
                // 流已在 send_event 的锁内装好，这里只起观察线程；
                // 若中途已被停止（broadcasting=false），线程首拍即自然退出
                if let Some((live, level, flag, first_cb, first_nonzero)) = watch.take() {
                    spawn_watch_thread(app, live, level, flag, first_cb, first_nonzero);
                }
            }
            Effect::AnnounceRecording => {
                let _ = app.emit(
                    "recording",
                    serde_json::json!({ "active": true, "model": display_model(cfg) }),
                );
            }
            Effect::StopAndTranscribe { cycle } => stop_and_transcribe(app, cfg, cycle),
            Effect::Deliver { outcome, cycle } => deliver(app, cfg, outcome, cycle),
            Effect::DropLateResult => println!("[stt] 迟到的识别结果已丢弃（周期已翻篇）"),
            Effect::FailOpen(e) => {
                eprintln!("[toggle] 开麦失败（已重试一次）：{e}");
                let _ = app.emit("result", serde_json::json!({ "error": e }));
            }
            Effect::DiscardStream => discard_stream(app, state),
            Effect::GoIdle => {
                let _ = app.emit("recording", serde_json::json!({ "active": false }));
                panel::hide_panel(app.clone());
            }
        }
    }
}

// 效果：开新篇——面板先进"唤醒中"，开麦在后台线程（可阻塞秒级，绝不许抱主线程）
fn spawn_opener(app: &AppHandle, cfg: &config::Config) {
    panel::show_panel(app);
    let _ = app.emit(
        "recording",
        serde_json::json!({ "active": true, "opening": true, "model": display_model(cfg) }),
    );
    let app2 = app.clone();
    // 开流配置缓存：命中则跳过设备能力枚举（分段计时见 /tmp/sayit-mic.log）
    let cache_path = app
        .path()
        .app_data_dir()
        .ok()
        .map(|d| d.join("mic-cache.json"));
    std::thread::spawn(move || {
        // 第一次失败先别急着报错：瞬态驱动故障（USB 声卡 StartIO 被拒之类）通常亚秒自愈，
        // 歇 500ms 重试一次；仍失败才以第一次的错误示人（第一次的更贴近根因）
        let open = || audio::start_with_cache(cache_path.as_deref());
        let handle = match open() {
            Ok(h) => h,
            Err(first) => {
                std::thread::sleep(Duration::from_millis(500));
                match open() {
                    Ok(h) => h,
                    Err(_) => {
                        send_event(&app2, crate::dictation::Event::MicFailed(first), None);
                        return;
                    }
                }
            }
        };
        send_event(&app2, crate::dictation::Event::MicReady, Some(handle));
    });
}

// 效果：起音量广播 + 开录判定线程（每 50ms 一拍）。流已在 send_event 的锁内装好。
// "正式开录" = 首个真实信号（RMS 越过死麦线），不是"第一个回调"——设备预热期
// 回调在跑但送零数据，此刻说的话不会被采样；"看到声纹=可以说话"才是可信承诺。
// 700ms 时间基准（2026-11-10 实测改的）：预热实测 435-570ms，原 2s 造成等待死锁。
// 宣布与否、宣布几次，全部由状态机裁定（重复信号在 Recording(已宣布) 下是 Ignored）
fn spawn_watch_thread(
    app: &AppHandle,
    live: std::sync::Arc<std::sync::atomic::AtomicBool>,
    level: std::sync::Arc<std::sync::atomic::AtomicU32>,
    flag: std::sync::Arc<std::sync::atomic::AtomicBool>,
    first_cb_ms: std::sync::Arc<std::sync::atomic::AtomicU32>,
    first_nonzero_ms: std::sync::Arc<std::sync::atomic::AtomicU32>,
) {
    let app3 = app.clone();
    std::thread::spawn(move || {
        let started = std::time::Instant::now();
        let mut signal_sent = false;
        let mut timeout_sent = false;
        while flag.load(Ordering::Relaxed) {
            let v = level.load(Ordering::Relaxed) as f32 / 1000.0;
            let _ = app3.emit("level", serde_json::json!({ "rms": v }));
            if !signal_sent && live.load(Ordering::Relaxed) {
                signal_sent = true;
                // 三拍分解：首回调（设备开始送数据）/ 首非零（麦克听到了东西）/ 越死麦线（RMS 达标）。
                // 首非零-首回调 ≈ 设备预热；越线-首非零 ≈ 用户开口反应——保温与兜底阈值靠这组数定
                crate::audio::mic_log(&format!(
                    "三拍：首回调 {}ms → 首非零 {}ms → 越死麦线 +{}ms",
                    first_cb_ms.load(Ordering::Relaxed),
                    first_nonzero_ms.load(Ordering::Relaxed),
                    started.elapsed().as_millis()
                ));
                send_event(&app3, crate::dictation::Event::Signal, None);
            }
            if !signal_sent && !timeout_sent && started.elapsed() >= Duration::from_millis(700) {
                // 时间基准的开录宣布（2026-11-10 三拍实测改的）：设备预热实测 435–570ms
                // （首非零-首回调），原 2s 兜底 + "等真信号"造成等待死锁——面板等用户
                // 开口、用户等声纹亮起，2.4s 的"越线延迟"全是用户在等红灯（训练效应）。
                // 700ms = 预热上限 + 余量：过了这村硬件必然在收声，红灯就是诚实的
                // "可以说话"；说话越线（Signal）仍会提前触发，谁先到谁宣布
                timeout_sent = true;
                send_event(&app3, crate::dictation::Event::ArmTimeout, None);
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    });
}

// 效果：停止并识别。静音结论与普通失败同形（Failed{reason})，同走 Deliver 通道
fn stop_and_transcribe(app: &AppHandle, cfg: &config::Config, cycle: u64) {
    let handle = {
        let state = app.state::<crate::state::SharedState>();
        let Ok(mut s) = state.lock() else { return };
        s.broadcasting.store(false, Ordering::Relaxed); // 音量线程收摊
        s.level.take();
        s.recording.take()
    };
    let Some(handle) = handle else {
        // 防御：装流与迁移已原子化，理论走不到这里；真走到也别把相位冻在 Transcribing
        eprintln!("[toggle] 停止时无流可收（异常时序），强制复位面板");
        let _ = app.emit("recording", serde_json::json!({ "active": false }));
        return;
    };

    let out_dir = app
        .path()
        .app_data_dir()
        .map(|d| d.join("recordings"))
        .unwrap_or_default();
    let (wav, secs, samples, peak) = match handle.stop(&out_dir) {
        Ok(x) => x,
        Err(e) => {
            let _ = app.emit("result", serde_json::json!({ "error": e }));
            return;
        }
    };

    // 整段录音一个非零样本都没有 = 全零假数据（权限被拒的麦只会送这个）。
    // 识别纯静音毫无意义，直接以静音结论交付——录音期间零打扰，结论在结果位说
    if peak < 0.0001 {
        println!("[toggle] 录音全程无声（峰值 0）：麦克风多半未授权/被占用");
        let _ = std::fs::remove_file(&wav); // 静音音频不留盘
        send_event(
            app,
            crate::dictation::Event::SttDone {
                cycle,
                outcome: crate::dictation::SttOutcome::Failed {
                    error: "没采到任何声音——麦克风可能未授权或被系统静音".into(),
                    reason: Some("silent_mic"),
                },
            },
            None,
        );
        return;
    }

    println!(
        "[toggle] 录音完成：{:.1}s / {} 原始采样 -> {}",
        secs,
        samples,
        wav.display()
    );
    let _ = app.emit("phase", serde_json::json!({ "stage": "transcribing" }));

    // 识别是重活（秒级阻塞），必须离开主线程：webview 的事件投递要过主线程事件循环。
    // 完成后把结果（连同出发时的周期号）交还状态机——翻篇与否由它裁定
    let cfg2 = cfg.clone();
    let app2 = app.clone();
    std::thread::spawn(move || {
        let started = std::time::Instant::now();
        let outcome = match crate::stt::transcribe(&app2, &cfg2, &wav) {
            Ok(text) => {
                println!(
                    "[stt] 识别完成，用时 {:.1}s：{}",
                    started.elapsed().as_secs_f32(),
                    text
                );
                crate::dictation::SttOutcome::Text(text)
            }
            Err(e) => {
                eprintln!("[stt] 识别失败：{e}");
                crate::dictation::SttOutcome::Failed { error: e, reason: None }
            }
        };
        send_event(&app2, crate::dictation::Event::SttDone { cycle, outcome }, None);
    });
}

// 状态机当前的周期号（deliver 的迟到守卫用；锁不可用时回 0 = 必然抑制，宁可少显示）
fn flow_cycle(app: &AppHandle) -> u64 {
    use tauri::Manager;
    let state = app.state::<crate::state::SharedState>();
    state.lock().map(|s| s.flow.cycle).unwrap_or(0)
}

// 效果：交付结果。焦点探测放独立线程（AX 是同步 XPC，目标 App 卡死不能拖住交付），
// 判定拿回后再回主线程注入（enigo→HIToolbox 是 main-thread-only）；失败进结果位。
// 迟到守卫：cycle0 取自状态机接受 SttDone 的时刻（Effect::Deliver 携带，唯一正确采样
// 点——deliver 入口再读会落进"锁释放→读"的缝隙）；stale 在主线程闭包内复读——热键
// 处理与该闭包同走主线程队列、彼此串行，复读值闭包执行期间不会再变。迟到的面板信号
// 会砸掉新现场（injected=true 直接收起录音中的面板），注入与剪贴板则照常（文字必交付）。
fn deliver(
    app: &AppHandle,
    cfg: &config::Config,
    outcome: crate::dictation::SttOutcome,
    cycle0: u64,
) {
    match outcome {
        crate::dictation::SttOutcome::Text(text) => {
            let mode = cfg.injection.mode.clone();
            let app3 = app.clone();
            // 记入历史（注入与否都记）——"文字不丢"的兜底：设置→历史，逐条显式复制
            crate::history::record_history(app, &text);
            std::thread::spawn(move || {
                let report = crate::focus::probe(Duration::from_millis(300));
                if report.verdict == crate::focus::Verdict::NoInput {
                    println!("[focus] 非输入态，跳过注入（{}）", report.basis);
                }
                let app4 = app3.clone();
                let _ = app3.run_on_main_thread(move || {
                    let stale = flow_cycle(&app4) != cycle0;
                    if stale {
                        println!("[focus] 交付迟到（周期已翻篇）：抑制面板事件，注入照常");
                    }
                    // 注入契约（2026-11-10 终稿 E + 剪贴板最小侵入）：确认非输入态
                    // （黑名单/AX 判定）才跳过注入并亮复制按钮——此时**不写剪贴板**
                    // （用户剪贴板里可能是自己的内容，不能霸占）；文字不丢由听写历史
                    // 保证（设置→历史，逐条显式复制），复制按钮点击时才写剪贴板。
                    // 判不出（AX 盲 App）照常注入：用户在此按键本身就是意图声明。
                    // 例外：辅助功能未授权时放行走注入路径——产出 NeedPermission 并自动带路
                    if report.verdict == crate::focus::Verdict::NoInput
                        && crate::inject::accessibility_ok()
                    {
                        if !stale {
                            let _ = app4.emit(
                                "result",
                                serde_json::json!({
                                    "text": text,
                                    "injected": false,
                                    "reason": "no_input_focus"
                                }),
                            );
                        }
                        return;
                    }
                    // 确认输入态（或未授权走注入兜底）：照常注入
                    let outcome = if mode == "type" {
                        crate::inject::type_text(&text)
                    } else {
                        crate::inject::paste_text(&text)
                    };
                    let (injected, reason) = match outcome {
                        crate::inject::InjectOutcome::Injected => (true, None),
                        crate::inject::InjectOutcome::NeedPermission => {
                            // 自动带路：系统不许可弹窗，但我们可以自动翻到正确的设置页
                            // （每次运行只自动开一次；翻开关那一下永远必须用户亲手+密码。
                            // 迟到交付不弹——别在新一轮录音中途抢焦点开设置页）
                            if !stale && !system::AUTO_OPENED_ACCESSIBILITY.swap(true, Ordering::Relaxed) {
                                let _ = system::open_accessibility_page(&app4);
                            }
                            (false, Some("accessibility".to_string()))
                        }
                        crate::inject::InjectOutcome::Failed(e) => (false, Some(e)),
                    };
                    if !stale {
                        let _ = app4.emit(
                            "result",
                            serde_json::json!({ "text": text, "injected": injected, "reason": reason }),
                        );
                    }
                });
            });
        }
        crate::dictation::SttOutcome::Failed { error, reason } => {
            let _ = app.emit(
                "result",
                serde_json::json!({ "error": error, "reason": reason }),
            );
        }
    }
}


// 效果：丢弃在装流（录音中被 Esc）。随事件附带的"迟到流"不进这里——
// 它从未采样，send_event 的收尾直接丢弃即可；这里只处理已采样的流：写盘即删（与旧实现一致）
fn discard_stream(app: &AppHandle, state: &State<'_, crate::state::SharedState>) {
    let handle = {
        let Ok(mut s) = state.lock() else { return };
        s.broadcasting.store(false, Ordering::Relaxed);
        s.level.take();
        s.recording.take()
    };
    if let Some(handle) = handle {
        println!("[toggle] 录音中止（Esc）");
        if let Ok(dir) = app.path().app_data_dir().map(|d| d.join("recordings")) {
            if let Ok((wav, _s, _n, _p)) = handle.stop(&dir) {
                let _ = std::fs::remove_file(&wav); // 半句话没说完，音频直接丢弃
            }
        }
    }
}
fn display_model(cfg: &Config) -> String {
    match cfg.stt.provider.as_str() {
        "openai_compatible" => format!("云端 · {}", cfg.stt.cloud.model),
        _ => format!(
            "本地 · {}",
            cfg.stt.local_model_id.clone().unwrap_or_else(|| "未选择".into())
        ),
    }
}
