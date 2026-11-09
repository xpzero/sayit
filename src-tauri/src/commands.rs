// 编排层：用例流程都在这里。快捷键按下 -> toggle_recording_impl 是全应用的心跳。
use crate::audio;
use crate::config::{self, Config};
use crate::engines;
use crate::hotkey;
use crate::models;
use std::sync::atomic::Ordering;
use std::time::Duration;
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
                show_panel(app);
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
                s.level = Some(level.clone());
                s.broadcasting.store(true, Ordering::Relaxed);
                let flag = s.broadcasting.clone();
                s.recording = Some(h);
                watch = Some((live, level, flag));
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

/// 观察三元组：(live 真信号标志, level 音量, broadcasting 运行开关)
type Watch = Option<(
    std::sync::Arc<std::sync::atomic::AtomicBool>,
    std::sync::Arc<std::sync::atomic::AtomicU32>,
    std::sync::Arc<std::sync::atomic::AtomicBool>,
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
                if let Some((live, level, flag)) = watch.take() {
                    spawn_watch_thread(app, live, level, flag);
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
                hide_panel(app.clone());
            }
        }
    }
}

// 效果：开新篇——面板先进"唤醒中"，开麦在后台线程（可阻塞秒级，绝不许抱主线程）
fn spawn_opener(app: &AppHandle, cfg: &config::Config) {
    show_panel(app);
    let _ = app.emit(
        "recording",
        serde_json::json!({ "active": true, "opening": true, "model": display_model(cfg) }),
    );
    let app2 = app.clone();
    std::thread::spawn(move || {
        // 第一次失败先别急着报错：瞬态驱动故障（USB 声卡 StartIO 被拒之类）通常亚秒自愈，
        // 歇 500ms 重试一次；仍失败才以第一次的错误示人（第一次的更贴近根因）
        let handle = match audio::start() {
            Ok(h) => h,
            Err(first) => {
                std::thread::sleep(Duration::from_millis(500));
                match audio::start() {
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
// 2s 兜底：设备只送全零（权限被拒）或首帧奇慢也不能永远卡在加载态。
// 宣布与否、宣布几次，全部由状态机裁定（重复信号在 Recording(已宣布) 下是 Ignored）
fn spawn_watch_thread(
    app: &AppHandle,
    live: std::sync::Arc<std::sync::atomic::AtomicBool>,
    level: std::sync::Arc<std::sync::atomic::AtomicU32>,
    flag: std::sync::Arc<std::sync::atomic::AtomicBool>,
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
                send_event(&app3, crate::dictation::Event::Signal, None);
            }
            if !signal_sent && !timeout_sent && started.elapsed() >= Duration::from_secs(2) {
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
                    // 契约（用户拍板）：只有确认在输入框才注入。非输入态/判不出都不注入，
                    // 文本进剪贴板 + 结果胶囊亮复制按钮让用户自己落位——AX 盲 App 里
                    // "真在输入框"与"非输入态"不可区分，一视同仁走按钮，文字不丢。
                    // 例外：辅助功能未授权时放行走注入路径——那里会产出 NeedPermission
                    // 并自动带路开设置（否则用户永远只看到复制按钮，不知道要先授权）
                    if report.verdict != crate::focus::Verdict::Input
                        && crate::inject::accessibility_ok()
                    {
                        // 剪贴板写失败要换徽标——不能一边失败一边承诺"文本在剪贴板"
                        let cb_failed = crate::inject::copy_to_clipboard(&text).is_err();
                        if cb_failed {
                            eprintln!("[inject] 非输入态回退写剪贴板失败");
                        }
                        let reason = if cb_failed {
                            "clipboard_failed"
                        } else if report.verdict == crate::focus::Verdict::NoInput {
                            "no_input_focus"
                        } else {
                            "focus_unknown"
                        };
                        if !stale {
                            let _ = app4.emit(
                                "result",
                                serde_json::json!({
                                    "text": text,
                                    "injected": false,
                                    "reason": reason
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
                            if !stale && !AUTO_OPENED_ACCESSIBILITY.swap(true, Ordering::Relaxed) {
                                let _ = open_accessibility_page(&app4);
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

// ---------- 输入焦点体检（取证仪器：判定 + AX 原始错误码 + 前台 App，见 focus.rs） ----------

#[tauri::command]
pub fn focus_selftest(app: AppHandle) {
    let app = app.clone();
    std::thread::spawn(move || {
        // 设置窗本身可聚焦——立即探测只会测到 Sayit 自己的按钮（结构上不可能报 Input）。
        // 先广播"等待"给用户 3 秒把焦点点进真正要测的现场，再探测
        let _ = app.emit("focus-selftest", serde_json::json!({ "stage": "waiting" }));
        std::thread::sleep(Duration::from_secs(3));
        // 体检给足死线（交付路径只给 300ms，这里要的是准确不是快）
        let report = crate::focus::probe(Duration::from_millis(1000));
        let _ = app.emit(
            "focus-selftest",
            serde_json::json!({
                "stage": "done",
                "verdict": format!("{:?}", report.verdict),
                "basis": report.basis,
                "frontmost": crate::inject::frontmost_app().map(|(b, _)| b),
                "axTrusted": crate::inject::accessibility_ok(),
            }),
        );
    });
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

#[tauri::command]
pub fn hide_panel(app: AppHandle) {
    if let Some(win) = app.get_webview_window("panel") {
        let _ = win.hide();
    }
}

// 打开设置页（可选直达某个 tab：rec/model/key/cloud/perm/about）
#[tauri::command]
pub fn open_settings(app: AppHandle, tab: Option<String>) {
    if let Some(win) = app.get_webview_window("settings") {
        let _ = win.show();
        let _ = win.set_focus();
        if let Some(t) = tab {
            let _ = app.emit("goto-tab", serde_json::json!({ "tab": t }));
        }
    }
}

// ---------- 麦克风自检（后台线程干活，完成后广播结果） ----------

#[tauri::command]
pub fn mic_selftest(app: AppHandle) {
    let app = app.clone();
    std::thread::spawn(move || {
        let _ = app.emit("selftest", serde_json::json!({ "stage": "recording" }));
        match audio::selftest(1500) {
            Ok(r) => {
                let verdict = if r.samples == 0 {
                    "设备没有送来任何数据（可能被占用或未授权）".to_string()
                } else if r.peak < 0.0001 {
                    "完全静音：macOS 很可能拒绝了麦克风权限（被拒的应用只会收到全零数据、不再弹窗）。\n请点旁边按钮打开系统设置 → 麦克风，给运行本应用的工具（终端/ZCode）打勾".to_string()
                } else if r.peak < 0.01 {
                    format!("能收到声音但很轻（峰值 {:.0}%）——离麦克风近一点再试", r.peak * 100.0)
                } else {
                    format!("麦克风工作正常 ✓ 峰值 {:.0}%，对着它说话试试快捷键", r.peak * 100.0)
                };
                let _ = app.emit(
                    "selftest",
                    serde_json::json!({ "stage": "done", "verdict": verdict, "ok": r.peak >= 0.01 && r.samples > 0 }),
                );
            }
            Err(e) => {
                let _ = app.emit(
                    "selftest",
                    serde_json::json!({ "stage": "error", "message": e }),
                );
            }
        }
    });
}

// 深链直达系统设置的麦克风授权页
#[tauri::command]
pub fn open_mic_settings(app: AppHandle) -> Result<(), String> {
    use tauri_plugin_opener::OpenerExt;
    app.opener()
        .open_url(
            "x-apple.systempreferences:com.apple.settings.PrivacySecurity.extension?Privacy_Microphone",
            None::<&str>,
        )
        .map_err(|e| e.to_string())
}

// ---------- 注入兜底（步骤 7） ----------

// 辅助功能授权状态（设置页权限体检用）
#[tauri::command]
pub fn accessibility_status() -> bool {
    crate::inject::accessibility_ok()
}

// 面板"复制"按钮：只写剪贴板
#[tauri::command]
pub fn copy_text(text: String) -> Result<(), String> {
    crate::inject::copy_to_clipboard(&text)
}

// 深链直达辅助功能授权页（模拟按键的前提）
#[tauri::command]
pub fn open_accessibility_settings(app: AppHandle) -> Result<(), String> {
    open_accessibility_page(&app)
}

fn open_accessibility_page(app: &AppHandle) -> Result<(), String> {
    use tauri_plugin_opener::OpenerExt;
    // 新版系统设置的隐私面板链接格式（旧格式 com.apple.preference.universalaccess
    // 会打开"辅助功能功能页"而不是授权列表——实测踩坑）
    app.opener()
        .open_url(
            "x-apple.systempreferences:com.apple.settings.PrivacySecurity.extension?Privacy_Accessibility",
            None::<&str>,
        )
        .map_err(|e| e.to_string())
}

// 自动带路的节流开关：每次运行只自动打开一次（反复弹设置页会烦人）
static AUTO_OPENED_ACCESSIBILITY: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

// ---------- 快捷键捕获 ----------

#[tauri::command]
pub fn start_hotkey_capture() {
    crate::hotkey::tap::start_capture();
}

#[tauri::command]
pub fn stop_hotkey_capture() {
    crate::hotkey::tap::stop_capture();
}

// 深链直达系统设置的键盘页（启用 Fn 需把 🌐 键行为设为"无操作"）
#[tauri::command]
pub fn open_keyboard_settings(app: AppHandle) -> Result<(), String> {
    use tauri_plugin_opener::OpenerExt;
    app.opener()
        .open_url(
            "x-apple.systempreferences:com.apple.preference.keyboard",
            None::<&str>,
        )
        .map_err(|e| e.to_string())
}

// ---------- 模型库（步骤 3） ----------

#[tauri::command]
pub fn get_model_library(app: AppHandle) -> Vec<models::LibraryItem> {
    models::library(&app)
}

#[tauri::command]
pub fn download_model(app: AppHandle, state: State<'_, crate::state::SharedState>, id: String) {
    let flag = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    {
        let s = state.lock().unwrap();
        let mut dls = s.downloads.lock().unwrap();
        // 同一个模型不重复开下
        if dls.contains_key(&id) {
            return;
        }
        dls.insert(id.clone(), flag.clone());
    }

    let app = app.clone();
    std::thread::spawn(move || {
        // 点击立即有反馈：先播报"开始连接"，绝不无声无息
        let _ = app.emit(
            "download-progress",
            serde_json::json!({ "id": id, "stage": "progress", "pct": 0, "note": "开始连接…" }),
        );
        // 防崩保护：线程内 panic 也要变成可见的错误事件
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            models::download(&app, &id, flag)
        }));
        match result {
            Ok(Ok(())) => {} // download() 内部已广播 done/cancelled
            Ok(Err(e)) => {
                let _ = app.emit(
                    "download-progress",
                    serde_json::json!({ "id": id, "stage": "error", "message": e }),
                );
            }
            Err(_) => {
                let _ = app.emit(
                    "download-progress",
                    serde_json::json!({ "id": id, "stage": "error", "message": "下载线程内部异常，请重试" }),
                );
            }
        }
        // 收摊：无论成败都摘掉取消标记（否则点第二次会被"已在下载"挡住）
        if let Some(state) = app.try_state::<crate::state::SharedState>() {
            if let Ok(s) = state.lock() {
                if let Ok(mut dls) = s.downloads.lock() {
                    dls.remove(&id);
                }
            }
        }
    });
}

#[tauri::command]
pub fn cancel_download(app: AppHandle, state: State<'_, crate::state::SharedState>, id: String) {
    if let Ok(s) = state.lock() {
        if let Ok(dls) = s.downloads.lock() {
            if let Some(flag) = dls.get(&id) {
                flag.store(true, Ordering::Relaxed);
            }
        }
    }
    let _ = app;
}

#[tauri::command]
pub fn delete_model(app: AppHandle, id: String) -> Result<(), String> {
    // 引擎可能正跑在这个模型上，先停掉
    crate::stt::local::kill_engine(&app);
    // 精选模型：删目录；自带模型：只从清单移除（文件是用户的，不动）
    if models::catalog().iter().any(|m| m.id == id) {
        let dir = app
            .path()
            .app_data_dir()
            .map_err(|e| e.to_string())?
            .join("models")
            .join(&id);
        std::fs::remove_dir_all(dir).map_err(|e| e.to_string())?;
    } else {
        let mut cfg = config::load(&app);
        cfg.model_library.retain(|e| e.id != id);
        if cfg.stt.local_model_id.as_deref() == Some(id.as_str()) {
            cfg.stt.local_model_id = None;
        }
        config::save(&app, &cfg)?;
    }
    Ok(())
}

#[tauri::command]
pub fn add_user_model(app: AppHandle, paths: Vec<String>) -> Result<String, String> {
    models::register_user_files(&app, &paths)
}

#[tauri::command]
pub fn set_default_model(app: AppHandle, id: String) -> Result<(), String> {
    // 只有"就绪"的模型才有资格当默认
    let item = models::library(&app)
        .into_iter()
        .find(|m| m.id == id && m.status == "ready")
        .ok_or("模型不存在或未就绪")?;
    let mut cfg = config::load(&app);
    if cfg.stt.local_model_id.as_deref() != Some(item.id.as_str()) {
        crate::stt::local::kill_engine(&app); // 换默认模型 = 引擎要换血
    }
    cfg.stt.local_model_id = Some(item.id);
    config::save(&app, &cfg)
}

// 面板窗口固定尺寸（520×170，见 tauri.conf.json）：一切状态切换都是胶囊在固定窗口内
// 的 CSS 动效。动态贴合内容的三种实现（瞬移/步进动画/透明帧掩护瞬移）均实测有肉眼
// 抖动——窗口服务 set_size/set_position 异步非原子 + WKWebView 陈旧帧拉伸/视口异步，
// 竞态砍不尽；固定几何是唯一稳定解（详见 AGENTS.md「面板窗口」条）。
const PANEL_BOTTOM_GAP: f64 = 96.0; // 距屏幕底部留白（避开 Dock 区域）

// 主屏底部居中摆位（先摆位再显示，避免闪跳）
fn center_panel_bottom(win: &tauri::WebviewWindow, w: f64, h: f64) {
    if let Ok(Some(mon)) = win.primary_monitor() {
        let sf = mon.scale_factor();
        let mw = mon.size().width as f64 / sf;
        let mh = mon.size().height as f64 / sf;
        let x = (mw - w) / 2.0;
        let y = (mh - h - PANEL_BOTTOM_GAP).max(0.0);
        let _ = win.set_position(tauri::LogicalPosition::new(x, y));
    }
}

/// 启动时摆一次面板位（窗口几何恒定，之后 show/hide 不再碰位置——show 路径零窗口操作）
pub fn position_panel(app: &AppHandle) {
    let Some(win) = app.get_webview_window("panel") else {
        return;
    };
    if let Ok(size) = win.outer_size() {
        let sf = win
            .primary_monitor()
            .ok()
            .flatten()
            .map(|m| m.scale_factor())
            .unwrap_or(2.0);
        center_panel_bottom(&win, size.width as f64 / sf, size.height as f64 / sf);
    }
}

fn show_panel(app: &AppHandle) {
    // 摆位只在启动时做一次（position_panel）；此处纯显示——面板出现的路径上
    // 没有任何 set_size/set_position，物理上不存在几何抖动源
    if let Some(win) = app.get_webview_window("panel") {
        let _ = win.show();
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
