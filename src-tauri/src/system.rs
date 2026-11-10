// 系统层杂项命令 —— 由 commands.rs 拆出（结构铁律 8：单一职责；搬家零行为变化）
// 麦克风自检、辅助功能查询/带路、系统设置深链、复制按钮的后盾——都是"与外部系统对话"的壳。
use tauri::{AppHandle, Emitter};

// ---------- 麦克风自检（后台线程干活，完成后广播结果） ----------

#[tauri::command]
pub fn mic_selftest(app: AppHandle) {
    let app = app.clone();
    std::thread::spawn(move || {
        let _ = app.emit("selftest", serde_json::json!({ "stage": "recording" }));
        match crate::audio::selftest(1500) {
            Ok(r) => {
                // 分档：nodata=设备无数据 / silent=全零（权限被拒的铁证）/ quiet=有声但轻
                // （本机麦说话峰值本就贴近 0.01 线，AGENTS.md 实测——quiet 是警告不是故障）
                // / ok=正常。前端按档配色显示，完整长诊断仍在 verdict 里悬停可读
                let (kind, verdict) = if r.samples == 0 {
                    ("nodata", "设备没有送来任何数据（可能被占用或未授权）".to_string())
                } else if r.peak < 0.0001 {
                    (
                        "silent",
                        "完全静音：macOS 很可能拒绝了麦克风权限（被拒的应用只会收到全零数据、不再弹窗）。\n请点旁边按钮打开系统设置 → 麦克风，给运行本应用的工具（终端/ZCode）打勾"
                            .to_string(),
                    )
                } else if r.peak < 0.01 {
                    (
                        "quiet",
                        format!("能收到声音但很轻（峰值 {:.0}%）——测试时要对着麦克风说话；平时听写正常就无碍", r.peak * 100.0),
                    )
                } else {
                    (
                        "ok",
                        format!("麦克风工作正常 ✓ 峰值 {:.0}%", r.peak * 100.0),
                    )
                };
                let _ = app.emit(
                    "selftest",
                    serde_json::json!({
                        "stage": "done",
                        "verdict": verdict,
                        "kind": kind,
                        "ok": kind == "ok",
                    }),
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

// ---------- 注入兜底 ----------

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

pub fn open_accessibility_page(app: &AppHandle) -> Result<(), String> {
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
pub(crate) static AUTO_OPENED_ACCESSIBILITY: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

// 权限门（perm.rs）的麦克风带路同款节流：每次运行只自动开一次麦克风设置页
pub(crate) static AUTO_OPENED_MIC: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

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
