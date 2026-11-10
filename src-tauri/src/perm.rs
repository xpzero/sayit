// 权限门：热键前置检查——麦克风被拒或辅助功能未开时，录音按键失效。
// 调用方 commands.rs toggle_core（开新篇路径）；带路与节流开关在 system.rs。
//
// 契约（2026-11-10 用户拍板，B 案）：静默拦 + 每次运行首次被拦时自动带路开对应设置页。
// - 麦克风"未询问"（notDetermined）必须放行：系统授权弹窗只在 App 第一次真正开麦时出现，
//   拦掉就永远拿不到弹窗（死锁）。只在明确拒绝（denied）或受限（restricted）时拦。
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

/// 麦克风授权状态是否放行。AVAuthorizationStatus：0 未询问 / 1 受限 / 2 拒绝 / 3 已授权。
/// 未询问放行（首开麦要触发系统弹窗）；未知值不冒充放行。
pub fn mic_status_allowed(status: i32) -> bool {
    status == 0 || status == 3
}

/// 权限门判定。辅助功能优先报（它拦得更彻底：热键注入全链路都依赖它）。
pub fn gate_decision(mic_allowed: bool, ax_trusted: bool) -> Option<Blocked> {
    if !ax_trusted {
        Some(Blocked::Accessibility)
    } else if !mic_allowed {
        Some(Blocked::Mic)
    } else {
        None
    }
}

// ---------- IO 壳 ----------

/// AVCaptureDevice.authorizationStatus(for: .audio)。查询失败返回 None（调用方按放行处理）。
/// 用 Class::get 而非 class! 宏：宏找不到类会直接 panic，这是按键路径，绝不能赌
/// AVFoundation 随 WebKit 传递加载一直成立（实测当前成立，2026-11-10 vmmap 验证）。
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

#[cfg(not(target_os = "macos"))]
pub fn mic_status() -> Option<i32> {
    Some(3) // 非 macOS 无此机制，视为已授权
}

/// 麦克风放行判定（IO 壳）。查询失败按放行（fail-open，见文件头）。
pub fn mic_allowed() -> bool {
    mic_status().map(mic_status_allowed).unwrap_or(true)
}

/// 权限门入口：每次开新篇的按键调用。None = 放行。
/// 观测点：原始状态值逐键落日志——authorizationStatus 测的是应用自身身份，
/// 看不见宿主终端（dev 归责）被拒（2026-11-11 TCC 日志实测），排查靠这行。
pub fn check() -> Option<Blocked> {
    let raw = mic_status();
    let blocked = gate_decision(raw.map(mic_status_allowed).unwrap_or(true), crate::inject::accessibility_ok());
    println!("[perm-gate] 麦克风原始状态 {raw:?}（None=查询失败）、辅助功能 {}，判定 {blocked:?}", 
        crate::inject::accessibility_ok());
    blocked
}

/// toggle 入口的权限门壳（commands.rs 调用）：仅开新篇相位（Idle/Transcribing）检查，
/// 其余相位放行——录音中的停止按键永不拦（防用户卡在录音态）。被拦时打 [perm-gate]
/// 日志，并按 B 案每次运行首次自动带路开对应设置页（system.rs 节流开关）。
/// 返回 true = 已拦下，调用方直接 return。
pub fn blocked_on_toggle(app: &tauri::AppHandle) -> bool {
    use std::sync::atomic::Ordering;
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
        Some(blocked) => {
            println!("[perm-gate] 按键拦截：{blocked:?}");
            match blocked {
                Blocked::Mic => {
                    if !crate::system::AUTO_OPENED_MIC.swap(true, Ordering::Relaxed) {
                        let _ = crate::system::open_mic_settings(app.clone());
                    }
                }
                Blocked::Accessibility => {
                    if !crate::system::AUTO_OPENED_ACCESSIBILITY.swap(true, Ordering::Relaxed) {
                        let _ = crate::system::open_accessibility_page(app);
                    }
                }
            }
            true
        }
        None => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 麦克风状态_未询问与已授权放行_其余拦截() {
        assert!(mic_status_allowed(0)); // 未询问：放行——首开麦触发系统弹窗，拦了就死锁
        assert!(mic_status_allowed(3)); // 已授权
        assert!(!mic_status_allowed(1)); // 受限
        assert!(!mic_status_allowed(2)); // 拒绝
        assert!(!mic_status_allowed(9)); // 未知值不冒充放行
    }

    #[test]
    fn 权限门_都过才放行_辅助功能优先报() {
        assert_eq!(gate_decision(true, true), None);
        assert_eq!(gate_decision(true, false), Some(Blocked::Accessibility));
        assert_eq!(gate_decision(false, true), Some(Blocked::Mic));
        // 双缺先报辅助功能（拦得更彻底的那项）
        assert_eq!(gate_decision(false, false), Some(Blocked::Accessibility));
    }
}
