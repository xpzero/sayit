// 权限门：热键前置检查——麦克风被拒或辅助功能未开时，录音按键失效。
// 调用方 commands.rs toggle_core（开新篇路径）。
//
// 契约（2026-11-10 B 案 → 2026-11-11 两次用户拍板修订）：
// - 未询问 → 授权弹窗先行（requestAccess，面板不上场）；允许 → 自动开录，拒绝 → 提示面板。
// - 麦克风"未询问"（notDetermined）：requestAccess 触发系统弹窗，面板不上场（消除
//   死麦早停与弹窗的竞态）；点允许接续按键意图自动开录，点拒绝弹 mic_denied 提示面板。
// - 麦克风明确拒绝/受限 → 提示面板（文案 + 去系统设置按钮，点击才跳）。
// - 辅助功能无"未询问"可言（系统不弹窗、只能用户手动开）：AXIsProcessTrusted()==false 即拦。
// - 查询是 TCC 缓存读（微秒级），每次按键实时查——系统设置里授权后无需重启。
// - 查询失败按放行（fail-open）：拦错会让按键全死；误放行最多浪费一次说话，
//   由死麦判定（silent_mic）/注入 NeedPermission 两条既有兜底接住。
// 观测点：commands.rs 的 [perm-gate] 日志。

/// 被拦项（带路目标与日志用）
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Blocked {
    Mic,
    Accessibility,
}

// ---------- 纯核心（断言见 mod tests） ----------

/// 权限门三值判定（断言见 mod tests）：
/// Pass = 放行；Prompt = 弹提示面板（文案与按钮见 Panel.tsx）；
/// RequestMic = 麦克风"未询问"——系统授权弹窗先行，面板不上场（2026-11-11 拍板）。
/// 辅助功能优先报（它拦得更彻底：热键注入全链路都依赖它）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GateAction {
    Pass,
    Prompt(Blocked),
    RequestMic,
}

pub fn gate_decision(mic_status: i32, ax_trusted: bool) -> GateAction {
    if !ax_trusted {
        return GateAction::Prompt(Blocked::Accessibility);
    }
    match mic_status {
        3 => GateAction::Pass,                // 已授权
        0 => GateAction::RequestMic,          // 未询问：requestAccess 弹窗先行
        _ => GateAction::Prompt(Blocked::Mic), // 1 受限 / 2 拒绝 / 未知值不冒充放行
    }
}

// ---------- IO 壳 ----------

/// AVCaptureDevice.authorizationStatus(for: .audio)。查询失败返回 None（调用方按放行处理）。
/// 也是命令：设置页权限体检格读它做三态分流（3 已授权/0 未询问/1、2 拒绝·受限）。
/// 用 Class::get 而非 class! 宏：宏找不到类会直接 panic，这是按键路径，绝不能赌
/// AVFoundation 随 WebKit 传递加载一直成立（实测当前成立，2026-11-10 vmmap 验证）。
#[tauri::command]
#[cfg(target_os = "macos")]
pub fn mic_status() -> Option<i32> {
    use objc2::{msg_send, runtime::Class, runtime::AnyObject};
    unsafe {
        // AVMediaTypeAudio == @"soun"
        let device_class = Class::get("AVCaptureDevice")?;
        let nsstring = Class::get("NSString")?;
        let c = std::ffi::CString::new("soun").ok()?;
        let media_type: *mut AnyObject =
            msg_send![nsstring, stringWithUTF8String: c.as_ptr()];
        if media_type.is_null() {
            return None;
        }
        // 返回类型必须是 isize：AVAuthorizationStatus 底层是 NSInteger（类型码 'q'），
        // 声明 i32 会被 objc2 运行时校验拒收（2026-11-10 按键崩溃的真因，panic.log 取证）
        let status: isize =
            msg_send![device_class, authorizationStatusForMediaType: media_type];
        Some(status as i32)
    }
}

#[tauri::command]
#[cfg(not(target_os = "macos"))]
pub fn mic_status() -> Option<i32> {
    Some(3) // 非 macOS 无此机制，视为已授权
}

/// 权限门入口：每次开新篇的按键调用。
/// 观测点：原始状态值逐键落日志（见文件头）；查询失败（None）按已授权放行（fail-open）。
pub fn check() -> GateAction {
    let raw = mic_status();
    let action = gate_decision(raw.unwrap_or(3), crate::inject::accessibility_ok());
    println!(
        "[perm-gate] 麦克风原始状态 {raw:?}（None=查询失败按已授权）、辅助功能 {}，判定 {action:?}",
        crate::inject::accessibility_ok()
    );
    action
}

/// toggle 入口的权限门壳（commands.rs 调用）：仅开新篇相位（Idle/Transcribing）检查，
/// 其余相位放行——录音中的停止按键永不拦（防用户卡在录音态）。被拦时打 [perm-gate]
/// 日志；被拦的处理见文件头契约（提示面板 / 授权弹窗先行）。
/// 返回 true = 已拦下，调用方直接 return。
pub fn blocked_on_toggle(app: &tauri::AppHandle) -> bool {
    use tauri::Manager;
    let opening_new = app
        .state::<crate::state::SharedState>()
        .lock()
        .map(|s| {
            matches!(
                s.flow.phase,
                crate::dictation::Phase::Idle | crate::dictation::Phase::Transcribing
            )
        })
        .unwrap_or(false); // 锁不到状态时放行（fail-open，与查询失败同哲学）
    if !opening_new {
        return false;
    }
    match check() {
        GateAction::Pass => false,
        GateAction::RequestMic => {
            println!("[perm-gate] 首次麦克风授权：系统弹窗先行（无面板）");
            request_mic_access(app);
            true
        }
        GateAction::Prompt(blocked) => {
            println!("[perm-gate] 按键拦截：{blocked:?}");
            // 先提示后带路（2026-11-11 用户拍板，改自 B 案的"静默拦+自动带路"）：
            // 弹面板亮"去开启"按钮，点击才跳系统设置——不再无解释地甩进设置页。
            // 不进状态机（no_model 同款先例）；反馈型提示无节流，每次被拦都亮
            use tauri::Emitter;
            let (error, reason) = match blocked {
                Blocked::Mic => ("麦克风权限被拒绝——Sayit 收不到声音", "mic_denied"),
                Blocked::Accessibility => (
                    "需要辅助功能权限：听写与插入文字都依赖它",
                    "accessibility",
                ),
            };
            crate::panel::show_panel(app);
            let _ = app.emit("result", serde_json::json!({ "error": error, "reason": reason }));
            true
        }
    }
}

/// 触发系统麦克风授权弹窗的公共底座（两条路径复用，objc 部分只此一份）：
/// - 热键路径 request_mic_access：允许 → 接续按键意图自动开录（toggle 全链路）
/// - 设置页 request_mic_access_quiet：仅广播 mic-perm 事件刷新状态，不自动开录
/// 在飞标志在此层：系统不重复弹，但 completionHandler 会叠加（两个回调先后触发
/// toggle 会把刚开的录音立即停掉）
static MIC_REQUEST_IN_FLIGHT: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

fn trigger_mic_request<F: Fn(bool) + Send + 'static>(on_result: F) {
    if MIC_REQUEST_IN_FLIGHT.swap(true, std::sync::atomic::Ordering::SeqCst) {
        return; // 弹窗已挂着：答复回调会把这次意图一并接上，不重发
    }
    use objc2::msg_send;
    use objc2::runtime::{AnyObject, Bool, Class};
    unsafe {
        let (Some(device_class), Some(nsstring)) =
            (Class::get("AVCaptureDevice"), Class::get("NSString"))
        else {
            MIC_REQUEST_IN_FLIGHT.store(false, std::sync::atomic::Ordering::SeqCst);
            eprintln!("[perm-gate] AVCaptureDevice 不可达，跳过授权请求（fail-open）");
            return;
        };
        let Ok(c) = std::ffi::CString::new("soun") else {
            MIC_REQUEST_IN_FLIGHT.store(false, std::sync::atomic::Ordering::SeqCst);
            return;
        };
        let media_type: *mut AnyObject = msg_send![nsstring, stringWithUTF8String: c.as_ptr()];
        if media_type.is_null() {
            MIC_REQUEST_IN_FLIGHT.store(false, std::sync::atomic::Ordering::SeqCst);
            return;
        }
        let handler = block2::RcBlock::new(move |granted: Bool| {
            MIC_REQUEST_IN_FLIGHT.store(false, std::sync::atomic::Ordering::SeqCst);
            println!("[perm-gate] 授权弹窗答复：{}", granted.as_bool());
            on_result(granted.as_bool());
        });
        let _: () = msg_send![
            device_class,
            requestAccessForMediaType: media_type
            completionHandler: &*handler
        ];
    }
}

/// 触发系统麦克风授权弹窗（不开麦、面板不上场）。答复在 completionHandler：
/// 允许 → 接续这次按键的意图直接开录（重走 toggle 全链路，此时已过门）；
/// 拒绝 → 弹 mic_denied 提示面板（与权限门同款文案）。
fn request_mic_access(app: &tauri::AppHandle) {
    let app = app.clone();
    trigger_mic_request(move |granted| {
        if granted {
            // 接续按键意图：重走 toggle 全链路（此时麦克风已授权，门直接放行）
            crate::commands::toggle_recording_impl(&app);
        } else {
            use tauri::Emitter;
            crate::panel::show_panel(&app);
            let _ = app.emit(
                "result",
                serde_json::json!({
                    "error": "麦克风权限被拒绝——Sayit 收不到声音",
                    "reason": "mic_denied"
                }),
            );
        }
    });
}

/// 设置页的授权请求（2026-11-11）：答复只广播 mic-perm 事件供设置页刷新状态——
/// 不自动开录（那是热键路径的语义）
#[tauri::command]
pub fn request_mic_permission(app: tauri::AppHandle) {
    let app = app.clone();
    trigger_mic_request(move |granted| {
        use tauri::Emitter;
        let _ = app.emit("mic-perm", serde_json::json!({ "granted": granted }));
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 权限门_三值判定与优先级() {
        use GateAction::*;
        // 已授权 → 放行
        assert_eq!(gate_decision(3, true), Pass);
        // 未询问 → 授权弹窗先行（2026-11-11：不再是"放行等首开麦弹窗"）
        assert_eq!(gate_decision(0, true), RequestMic);
        // 受限 / 拒绝 / 未知值 → 提示面板，未知值不冒充放行
        assert_eq!(gate_decision(1, true), Prompt(Blocked::Mic));
        assert_eq!(gate_decision(2, true), Prompt(Blocked::Mic));
        assert_eq!(gate_decision(9, true), Prompt(Blocked::Mic));
        // 双缺先报辅助功能（拦得更彻底的那项）
        assert_eq!(gate_decision(0, false), Prompt(Blocked::Accessibility));
        assert_eq!(gate_decision(2, false), Prompt(Blocked::Accessibility));
    }
}
