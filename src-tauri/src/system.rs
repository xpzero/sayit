// 系统层杂项命令 —— 由 commands.rs 拆出（结构铁律 8：单一职责；搬家零行为变化）
// 麦克风自检、辅助功能查询/带路、系统设置深链、复制按钮的后盾——都是"与外部系统对话"的壳。
use tauri::{AppHandle, Emitter};


// ---------- App Nap 抑制（2026-11-11 打包版实测补） ----------
// 托盘常驻 + 面板 focusable:false（永远非键窗）→ 空闲时被系统 Nap（日志实证：
// WebContent 每 ~10s 在 nap 间循环），result 事件要先唤醒 webview 才收面板——
// 打包版"面板消失慢"的来源。dev 挂在终端下不会被 Nap，所以 dev 里无感。
// 持有 NSActivityUserInitiatedAllowingIdleSystemSleep（0x00FFFF00）：防 Nap 但
// 仍允许系统睡眠——听写工具不该阻止合盖休眠。返回的 activity 对象进程同寿持有。
pub fn suppress_app_nap() {
    use objc2::{msg_send, runtime::AnyObject, runtime::Class};
    let (Some(pi_cls), Some(nsstring_cls)) = (Class::get("NSProcessInfo"), Class::get("NSString")) else {
        eprintln!("[nap] 类不可达（AVFoundation 未加载？），跳过 App Nap 抑制");
        return;
    };
    let Ok(reason) = std::ffi::CString::new("sayit 常驻听写：热键响应与面板收起不进 nap") else {
        return;
    };
    unsafe {
        let pi: *mut AnyObject = msg_send![pi_cls, processInfo];
        let nsreason: *mut AnyObject = msg_send![nsstring_cls, stringWithUTF8String: reason.as_ptr()];
        // NSActivityUserInitiatedAllowingIdleSystemSleep（NSProcessInfo.h：
        // NSActivityUserInitiated = 0x00FFFFFF | IdleSystemSleep；去掉 IdleSystemSleep = 0x00FFFF00）
        let activity: *mut core::ffi::c_void = msg_send![
            pi,
            beginActivityWithOptions: 0x00FF_FF00u64
            reason: nsreason
        ];
        if activity.is_null() {
            eprintln!("[nap] beginActivity 失败（App Nap 未抑制，面板可能偏慢）");
        } else {
            std::mem::forget(activity); // end 即失效：进程生命周期持有，永不结束
            println!("[nap] App Nap 已抑制（允许系统正常睡眠）");
        }
    }
}

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
pub fn copy_text(app: AppHandle, text: String) -> Result<(), String> {
    let t0 = std::time::Instant::now();
    let r = crate::inject::copy_to_clipboard(&text);
    println!("[copy] 写剪贴板耗时 {}ms", t0.elapsed().as_millis());
    if let Err(e) = &r {
        // 写失败不能无声（剪贴板最小侵入契约的失败可见条款）：面板回弹亮错。
        // 面板路径前端已先行收板（不等复制），失败由这里拉回；听写历史等其它调用方
        // 共享此兑底（面板弹出说明系统剪贴板异常，用户需要知道）
        crate::panel::show_panel(&app);
        let _ = app.emit(
            "result",
            serde_json::json!({ "error": format!("复制失败：{e}"), "reason": "copy_fail" }),
        );
    }
    r
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

// 自动带路的节流开关：每次运行只自动打开一次（反复弹设置页会烦人）。
// 仅剩死麦带路（deliver Failed/silent_mic）在用；权限门与 NeedPermission 已改提示式
// （2026-11-11），不再自动跳设置页
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
