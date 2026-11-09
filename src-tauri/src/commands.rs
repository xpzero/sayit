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

// ---------- 录音开关（快捷键和界面按钮都走这里） ----------

// 供快捷键 handler 调用（无 State 参数版本的入口）
pub fn toggle_recording_impl(app: &AppHandle) {
    let state = app.state::<crate::state::SharedState>();
    if let Err(e) = toggle_core(app, &state) {
        eprintln!("[toggle] {e}");
        // 失败也要让用户看见：面板弹出来显示错误
        show_panel(app);
        let _ = app.emit(
            "result",
            serde_json::json!({ "error": e }),
        );
    }
}

#[tauri::command]
pub fn toggle_recording(app: AppHandle, state: State<'_, crate::state::SharedState>) {
    if let Err(e) = toggle_core(&app, &state) {
        show_panel(&app);
        let _ = app.emit("result", serde_json::json!({ "error": e }));
    }
}

fn toggle_core(app: &AppHandle, state: &State<'_, crate::state::SharedState>) -> Result<(), String> {
    let cfg = config::load(app);
    let mut s = state.lock().map_err(|e| e.to_string())?;

    if let Some(handle) = s.recording.take() {
        // ---- 停止：收桶 -> 打包 WAV -> 识别交给后台线程 ----
        s.broadcasting.store(false, Ordering::Relaxed);
        RECORDING.store(false, Ordering::Relaxed);
        let level_slot = s.level.take();
        drop(s); // 不抱着锁干活

        let out_dir = app
            .path()
            .app_data_dir()
            .map_err(|e| e.to_string())?
            .join("recordings");
        let (wav, secs, samples, peak) = handle.stop(&out_dir)?;
        drop(level_slot);

        // 整段录音一个非零样本都没有 = 全零假数据（权限被拒的麦只会送这个）。
        // 识别纯静音毫无意义，直接给结论——死麦判定归位到停止时刻，录音期间不打扰
        if peak < 0.0001 {
            println!("[toggle] 录音全程无声（峰值 0）：麦克风多半未授权/被占用");
            let _ = std::fs::remove_file(&wav); // 静音音频不留盘
            let _ = app.emit(
                "result",
                serde_json::json!({
                    "error": "没采到任何声音——麦克风可能未授权或被系统静音",
                    "reason": "silent_mic"
                }),
            );
            return Ok(());
        }

        println!(
            "[toggle] 录音完成：{:.1}s / {} 原始采样 -> {}",
            secs,
            samples,
            wav.display()
        );
        let _ = app.emit("phase", serde_json::json!({ "stage": "transcribing" }));

        // 识别是重活（秒级阻塞），必须离开主线程：webview 的事件投递要过主线程事件循环，
        // 主线程被识别堵住的话"识别中"事件永远到不了页面（现象：加载态从未出现）。
        // 注入恰好相反——enigo→HIToolbox 是 main-thread-only，识别完再派回主线程。
        let cfg2 = config::load(app);
        let my_cycle = CYCLE.load(Ordering::Relaxed); // 结果只属于本次听写：期间有人开了新篇就作废
        let app2 = app.clone();
        std::thread::spawn(move || {
            let started = std::time::Instant::now();
            let stale = || CYCLE.load(Ordering::Relaxed) != my_cycle;
            match crate::stt::transcribe(&app2, &cfg2, &wav) {
                Ok(text) => {
                    if stale() {
                        println!(
                            "[stt] 识别完成（用时 {:.1}s）但已被新一轮听写取代，结果丢弃",
                            started.elapsed().as_secs_f32()
                        );
                        return; // 迟到的注入会污染新现场，什么都不做才是对的
                    }
                    println!(
                        "[stt] 识别完成，用时 {:.1}s：{}",
                        started.elapsed().as_secs_f32(),
                        text
                    );
                    // 识别成功 -> 注入光标处（主线程）+ 广播结果
                    let app3 = app2.clone();
                    let _ = app2.run_on_main_thread(move || {
                        let outcome = if cfg2.injection.mode == "type" {
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
                Err(e) => {
                    if stale() {
                        return; // 翻篇后的错误同样不必打扰新现场
                    }
                    eprintln!("[stt] 识别失败：{e}");
                    let _ = app2.emit("result", serde_json::json!({ "error": e }));
                }
            }
        });
        Ok(())
    } else {
        // ---- 前置检查：本地识别却没有就绪模型 → 不录了（免得用户白说一段话），直接带路 ----
        if cfg.stt.provider != "openai_compatible" {
            let ready = crate::models::library(app)
                .into_iter()
                .any(|m| m.id == cfg.stt.local_model_id.clone().unwrap_or_default() && m.status == "ready");
            if !ready {
                show_panel(app);
                let _ = app.emit(
                    "result",
                    serde_json::json!({ "error": "还没有可用的识别模型", "reason": "no_model" }),
                );
                return Ok(());
            }
        }

        // ---- 开始：先见面板（加载态）-> 后台开麦 -> 正式开录后再亮声纹 ----
        // audio::start() 可阻塞秒级（USB/蓝牙唤醒实测 ~0.6s，蓝牙更久），绝不能抱着主线程：
        // 面板渲染和事件投递都要过主线程事件循环。开麦挪进后台线程，面板先显示"唤醒中"。
        // OPENING 闩锁：开麦进行中忽略重复触发（此时既没有流可停，也不该开第二个）。
        if OPENING.swap(true, Ordering::Relaxed) {
            return Ok(());
        }
        drop(s); // 启动路径拿锁只为判断"是否有流在录"；开麦本身不再需要它
        // 新听写开篇 = 上一段还在跑的识别就此作废（用户已翻篇）
        CYCLE.fetch_add(1, Ordering::Relaxed);
        // RECORDING 先置位再开麦：等待期间按 Esc 也要能中止
        RECORDING.store(true, Ordering::Relaxed);

        show_panel(app);
        let _ = app.emit(
            "recording",
            serde_json::json!({ "active": true, "opening": true, "model": display_model(&cfg) }),
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
                            OPENING.store(false, Ordering::Relaxed);
                            RECORDING.store(false, Ordering::Relaxed);
                            eprintln!("[toggle] 开麦失败（已重试一次）：{first}");
                            // 面板已可见，错误直接进结果位（与旧路径同样的可见性）
                            let _ = app2.emit("result", serde_json::json!({ "error": first }));
                            return;
                        }
                    }
                }
            };

            let state = app2.state::<crate::state::SharedState>();
            let Ok(mut s) = state.lock() else {
                drop(handle);
                OPENING.store(false, Ordering::Relaxed);
                RECORDING.store(false, Ordering::Relaxed);
                return;
            };
            // 等待期间被 Esc 中止：刚拿到手的流直接丢弃，不留状态、不留文件
            if !RECORDING.load(Ordering::Relaxed) {
                drop(s);
                drop(handle);
                OPENING.store(false, Ordering::Relaxed);
                return;
            }
            s.level = Some(handle.level.clone());
            s.broadcasting.store(true, Ordering::Relaxed);
            let flag = s.broadcasting.clone();
            let level = handle.level.clone();
            let live = handle.live.clone();
            s.recording = Some(handle);
            // 撤闩要在锁内（stop 分支拿同一把锁，此后触发/停止判定就不会错位）
            OPENING.store(false, Ordering::Relaxed);
            drop(s); // 不抱着锁干活

            // 音量广播 + 开录判定，二合一（每 50ms 一个节拍）：
            // 1) 开麦期间照常推音量（面板在加载态，暂不消费）；
            // 2) "正式开录"（面板加载态切声纹）= 首个真实信号（RMS 超过死麦线）。注意不是
            //    "第一个回调"：play() 成功后回调就在跑，但设备预热期送的是零数据（本机实测
            //    再 ~0.5s 才来真信号），此刻说的话不会被采样——"看到声纹=可以说话"才是
            //    可信承诺。2 秒兜底：设备只送全零（权限被拒）或首帧奇慢也不能永远卡在加载态。
            // 录音期间不做任何"没声音"式打扰——录多久、说不说是用户的事；死麦（全零假数据）
            // 的判定归位到停止时刻（见 stop 分支的 peak 检查），结论在结果位说。
            let model = display_model(&cfg);
            let app3 = app2.clone();
            std::thread::spawn(move || {
                let started = std::time::Instant::now();
                let mut armed = false; // 已发出"可以说话"的承诺（真实信号或 2s 兜底）
                while flag.load(Ordering::Relaxed) {
                    let v = level.load(Ordering::Relaxed) as f32 / 1000.0;
                    let _ = app3.emit("level", serde_json::json!({ "rms": v }));
                    if !armed
                        && (live.load(Ordering::Relaxed)
                            || started.elapsed() >= Duration::from_secs(2))
                    {
                        armed = true;
                        let _ = app3.emit(
                            "recording",
                            serde_json::json!({ "active": true, "model": model }),
                        );
                    }
                    std::thread::sleep(Duration::from_millis(50));
                }
            });
        });
        Ok(())
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

// 录音中途放弃（Esc）：收麦但不识别、不留音频、面板收起
pub fn abort_recording_impl(app: &AppHandle) {
    RECORDING.store(false, Ordering::Relaxed);
    SHOW_PENDING.store(false, Ordering::Relaxed); // 兜底显示线程不得复活已中止的面板
    let state = app.state::<crate::state::SharedState>();
    let Ok(mut s) = state.lock() else { return };
    if let Some(handle) = s.recording.take() {
        s.broadcasting.store(false, Ordering::Relaxed);
        s.level.take();
        drop(s); // 不抱着锁干活

        if let Ok(dir) = app.path().app_data_dir().map(|d| d.join("recordings")) {
            if let Ok((wav, _secs, _samples, _peak)) = handle.stop(&dir) {
                let _ = std::fs::remove_file(&wav); // 半句话没说完，音频直接丢弃
            }
        }
        println!("[toggle] 录音中止（Esc）");
    }
    let _ = app.emit("recording", serde_json::json!({ "active": false }));
    if let Some(win) = app.get_webview_window("panel") {
        let _ = win.hide();
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

// 录音进行中的无锁标志：tap 回调要读它（Esc 中止判断），不能去碰 SharedState 的互斥锁——
// 回调等锁超时会被系统禁用 tap。开麦/收麦期间它也兼作"等待中"状态位
pub(crate) static RECORDING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

// 开麦进行中（audio::start 在后台线程跑）：闩住重复触发；由开麦线程独占置位/复位
static OPENING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

// 听写周期号：每开始一次新听写 +1。识别是后台慢活（本地可到数十秒），完成时若发现
// 周期已翻篇，结果与错误一概丢弃——迟到的注入会把旧文字敲进用户正在用的新输入现场，
// 结果事件还会把面板从新录音态硬掰走（实测：连续两段听写间隔 2 秒即可复现）
static CYCLE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

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
