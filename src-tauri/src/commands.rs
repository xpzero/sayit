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
    // "保存即应用"：快捷键立即重注册
    hotkey::register_all(&app)?;
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
        // ---- 停止：收桶 -> 打包 WAV -> （步骤4 起交给识别组，现在占位） ----
        s.broadcasting.store(false, Ordering::Relaxed);
        let level_slot = s.level.take();
        drop(s); // 不抱着锁干活

        let out_dir = app
            .path()
            .app_data_dir()
            .map_err(|e| e.to_string())?
            .join("recordings");
        let (wav, secs, samples) = handle.stop(&out_dir)?;
        drop(level_slot);

        println!(
            "[toggle] 录音完成：{:.1}s / {} 原始采样 -> {}",
            secs,
            samples,
            wav.display()
        );
        let _ = app.emit("phase", serde_json::json!({ "stage": "transcribing" }));

        // 真正的识别来了（步骤 4）：拿最新配置分发到对应插头
        let cfg2 = config::load(app);
        let started = std::time::Instant::now();
        match crate::stt::transcribe(app, &cfg2, &wav) {
            Ok(text) => {
                println!(
                    "[stt] 识别完成，用时 {:.1}s：{}",
                    started.elapsed().as_secs_f32(),
                    text
                );

                // 识别成功 -> 尝试注入光标处（第 5 讲：粘贴无回执，复制按钮永远兜底）
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
                            let _ = open_accessibility_page(app);
                        }
                        (false, Some("accessibility".to_string()))
                    }
                    crate::inject::InjectOutcome::Failed(e) => (false, Some(e)),
                };
                let _ = app.emit(
                    "result",
                    serde_json::json!({ "text": text, "injected": injected, "reason": reason }),
                );
            }
            Err(e) => {
                eprintln!("[stt] 识别失败：{e}");
                let _ = app.emit("result", serde_json::json!({ "error": e }));
            }
        }
        Ok(())
    } else {
        // ---- 开始：开麦 -> 唤起面板 ----
        let handle = audio::start()?;
        s.level = Some(handle.level.clone());
        s.broadcasting.store(true, Ordering::Relaxed);
        let flag = s.broadcasting.clone();
        let level = handle.level.clone();
        s.recording = Some(handle);
        drop(s); // 不抱着锁干活

        // 音量广播线程：每 50ms 推音量；附带静音检测（连续 2 秒无声 → 警告一次）
        // 这是麦克风攻坚战换来的教训：权限被拒时 macOS 给"全零假数据"，必须主动识破
        let app2 = app.clone();
        std::thread::spawn(move || {
            let mut silent_since = std::time::Instant::now();
            let mut warned = false;
            while flag.load(Ordering::Relaxed) {
                let v = level.load(Ordering::Relaxed) as f32 / 1000.0;
                let _ = app2.emit("level", serde_json::json!({ "rms": v }));
                if v > 0.006 {
                    if warned {
                        let _ = app2.emit("silence", serde_json::json!({ "active": false }));
                        warned = false;
                    }
                    silent_since = std::time::Instant::now();
                } else if !warned && silent_since.elapsed() >= Duration::from_secs(2) {
                    let _ = app2.emit("silence", serde_json::json!({ "active": true }));
                    warned = true;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
        });

        show_panel(app);
        let _ = app.emit(
            "recording",
            serde_json::json!({ "active": true, "model": display_model(&cfg) }),
        );
        Ok(())
    }
}

#[tauri::command]
pub fn hide_panel(app: AppHandle) {
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

// ---------- 快捷键捕获（步骤 6） ----------

#[tauri::command]
pub fn start_hotkey_capture() {
    crate::hotkey::fn_listener::start_capture();
}

#[tauri::command]
pub fn stop_hotkey_capture() {
    crate::hotkey::fn_listener::stop_capture();
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

// 面板显示在主屏底部居中（先摆位再显示，避免闪跳）
fn show_panel(app: &AppHandle) {
    let Some(win) = app.get_webview_window("panel") else {
        return;
    };
    const PANEL_W: f64 = 520.0;
    const PANEL_H: f64 = 170.0;
    if let Ok(Some(mon)) = win.primary_monitor() {
        let sf = mon.scale_factor();
        let mw = mon.size().width as f64 / sf;
        let mh = mon.size().height as f64 / sf;
        let x = (mw - PANEL_W) / 2.0;
        let y = (mh - PANEL_H - 96.0).max(0.0);
        let _ = win.set_position(tauri::LogicalPosition::new(x, y));
    }
    let _ = win.show();
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
