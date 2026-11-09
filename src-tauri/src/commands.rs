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
            Effect::Deliver(outcome) => deliver(app, cfg, outcome),
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

// 效果：交付结果。注入走主线程（enigo→HIToolbox 是 main-thread-only）；失败进结果位
fn deliver(app: &AppHandle, cfg: &config::Config, outcome: crate::dictation::SttOutcome) {
    match outcome {
        crate::dictation::SttOutcome::Text(text) => {
            let mode = cfg.injection.mode.clone();
            let app3 = app.clone();
            let _ = app.run_on_main_thread(move || {
                // 前台是 Finder（桌面场景）⌘V 只会打进虚空、注入还会谎报成功——
                // 不注入，文本进剪贴板 + 结果胶囊亮复制按钮让用户自己落位。
                // 判定走 NSWorkspace（无需权限，见 inject.rs），任何失败一律放行注入
                if crate::inject::frontmost_is_finder() {
                    // 剪贴板写失败要换徽标——不能一边失败一边承诺"文本在剪贴板"
                    let cb_failed = crate::inject::copy_to_clipboard(&text).is_err();
                    if cb_failed {
                        eprintln!("[inject] 桌面回退写剪贴板失败");
                    }
                    let _ = app3.emit(
                        "result",
                        serde_json::json!({
                            "text": text,
                            "injected": false,
                            "reason": if cb_failed { "clipboard_failed" } else { "no_input_focus" }
                        }),
                    );
                    return;
                }
                let outcome = if mode == "type" {
                    crate::inject::type_text(&text)
                } else {
                    crate::inject::paste_text(&text)
                };
                let (injected, reason) = match outcome {
                    crate::inject::InjectOutcome::Injected => (true, None),
                    crate::inject::InjectOutcome::NeedPermission => {
                        // 自动带路：系统不许可弹窗，但我们可以自动翻到正确的设置页
                        // （每次运行只自动开一次；翻开关那一下永远必须用户亲手+密码）
                        if !AUTO_OPENED_ACCESSIBILITY.swap(true, Ordering::Relaxed) {
                            let _ = open_accessibility_page(&app3);
                        }
                        (false, Some("accessibility".to_string()))
                    }
                    crate::inject::InjectOutcome::Failed(e) => (false, Some(e)),
                };
                let _ = app3.emit(
                    "result",
                    serde_json::json!({ "text": text, "injected": injected, "reason": reason }),
                );
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

#[tauri::command]
pub fn hide_panel(app: AppHandle) {
    SHOW_PENDING.store(false, Ordering::Relaxed); // 兜底显示线程不得复活已隐藏的面板
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

// 面板窗口贴合内容：胶囊多大、窗口就多大（唯一可见的"窗口"就是胶囊本身）
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

// 出场防闪帧：show_panel 不再直接显示，而是等第一次尺寸贴合完成（内容已按新状态渲染、
// 窗口已瞬移到正确尺寸）再 show——否则会先以旧尺寸（上次的结果胶囊）闪一帧再跳变。
static SHOW_PENDING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

fn show_panel(app: &AppHandle) {
    let Some(win) = app.get_webview_window("panel") else {
        return;
    };
    if !win.is_visible().unwrap_or(false) {
        SHOW_PENDING.store(true, std::sync::atomic::Ordering::Relaxed);
        // 兜底：内容迟迟不报尺寸（隐藏 webview 可能不排版）就按当前尺寸显示，绝不能永远藏着
        let app2 = app.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(250));
            if SHOW_PENDING.swap(false, Ordering::Relaxed) {
                if let Some(win) = app2.get_webview_window("panel") {
                    if let Ok(size) = win.outer_size() {
                        let sf = win
                            .primary_monitor()
                            .ok()
                            .flatten()
                            .map(|m| m.scale_factor())
                            .unwrap_or(2.0);
                        center_panel_bottom(&win, size.width as f64 / sf, size.height as f64 / sf);
                    }
                    let _ = win.show();
                }
            }
        });
    } else {
        // 已可见（如结果展示中再按热键）：直接摆位即可，尺寸贴合走瞬移
        if let Ok(size) = win.outer_size() {
            let sf = win
                .primary_monitor()
                .ok()
                .flatten()
                .map(|m| m.scale_factor())
                .unwrap_or(2.0);
            center_panel_bottom(&win, size.width as f64 / sf, size.height as f64 / sf);
        }
        let _ = win.show();
    }
}

// 前端 ResizeObserver 报来内容尺寸（CSS 逻辑像素），窗口一步贴合到位。
// 刻意不做步进动画：曾试过 6 步 easeOut 渐变（每步 18ms 各调一次 set_size + set_position），
// 窗口服务与 DOM 重排各走各的节拍，步进途中窗口尺寸 ≠ 胶囊自然尺寸，flex 居中让
// 胶囊在屏上每步挪一点 = 状态切换时的肉眼抖动。窗口本身透明无边框、边缘不可见，
// 瞬移是唯一不抖的做法。
#[tauri::command]
pub fn resize_panel(app: AppHandle, w: f64, h: f64) {
    let Some(win) = app.get_webview_window("panel") else {
        return;
    };
    let w = w.max(40.0);
    let h = h.max(40.0);
    let _ = win.set_size(tauri::LogicalSize::new(w, h));
    center_panel_bottom(&win, w, h);
    // 出场流程的收尾：尺寸已就位，此刻显示就不会闪旧帧
    if SHOW_PENDING.swap(false, std::sync::atomic::Ordering::Relaxed) {
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
