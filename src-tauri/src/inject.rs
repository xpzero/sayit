// 文字注入模块：把识别文字送进"当前光标所在的应用"
// 主方案 = 写剪贴板 + 模拟 Cmd+V（结果常驻剪贴板，不备份/还原——见 paste_text 内注释）
// 备选   = 直接键入：合成携带 Unicode 文本的输入事件（剪贴板里也留一份兜底）
//
// 历史教训（2026-11-09 实测）：dev 二进制重编译后 TCC 授权与代码签名失配，
// AXIsProcessTrusted() 谎报 true 而 AX 查询失败——事件注入（enigo→CGEventPost）走另一
// 套检查不受影响。当年记录错误码为"-25212 (API disabled)"，SDK 头文件核对后勘误：
// -25211 才是 APIDisabled，-25212 是 NoValue。焦点探测如今走 focus.rs 分层链——注入
// 契约：只有确认 Input（含终端判定）才注入，NoInput/Unknown 不注入 + 复制按钮；
// dev 签名固定方案见 scripts/dev-runner.sh。
pub enum InjectOutcome {
    Injected,
    /// 辅助功能权限未开——模拟按键会被系统静默丢弃（不报错！），必须主动检测
    NeedPermission,
    Failed(String),
}

// macOS 辅助功能授权查询（ApplicationServices 框架）
#[cfg(target_os = "macos")]
#[link(name = "ApplicationServices", kind = "framework")]
unsafe extern "C" {
    fn AXIsProcessTrusted() -> bool;
}

pub fn accessibility_ok() -> bool {
    #[cfg(target_os = "macos")]
    unsafe {
        AXIsProcessTrusted()
    }
    #[cfg(not(target_os = "macos"))]
    true
}

// 前台应用（bundle id + pid，focus.rs 名单兜底/终端判定与体检用）：NSWorkspace 是普通
// AppKit 信息，**不需要辅助功能权限**——AX 通道之外唯一可靠的"现在哪个 App 在最前面"信号。
// 边界（明示的取舍）：它只知道前台是谁，不知道焦点是不是文本框（那个信息只有 AX 有）。
#[cfg(target_os = "macos")]
pub fn frontmost_app() -> Option<(String, i32)> {
    use objc2::{class, msg_send, runtime::AnyObject};
    unsafe {
        let ws: *mut AnyObject = msg_send![class!(NSWorkspace), sharedWorkspace];
        if ws.is_null() {
            return None;
        }
        let app: *mut AnyObject = msg_send![ws, frontmostApplication];
        if app.is_null() {
            return None;
        }
        let bid: *mut AnyObject = msg_send![app, bundleIdentifier];
        if bid.is_null() {
            return None;
        }
        let utf8: *const std::ffi::c_char = msg_send![bid, UTF8String];
        if utf8.is_null() {
            return None;
        }
        let pid: i32 = msg_send![app, processIdentifier];
        Some((std::ffi::CStr::from_ptr(utf8).to_string_lossy().into_owned(), pid))
    }
}

#[cfg(not(target_os = "macos"))]
pub fn frontmost_app() -> Option<(String, i32)> {
    None
}

pub fn paste_text(text: &str) -> InjectOutcome {
    if !accessibility_ok() {
        return InjectOutcome::NeedPermission;
    }
    let mut cb = match arboard::Clipboard::new() {
        Ok(c) => c,
        Err(e) => return InjectOutcome::Failed(format!("打不开剪贴板：{e}")),
    };

    // 识别结果常驻剪贴板，不做备份/还原。三个理由：
    // 1) Cmd+V 落不进任何输入框时（焦点在桌面等），文本仍在剪贴板——点进输入框按
    //    ⌘V 即可，任何情况下文字都不会丢；
    // 2) 备份/还原有先天竞态：还原太早，目标应用（网页/Electron 的慢事件循环）还没
    //    读剪贴板，用户粘到的反而是"旧内容"——实测踩坑；
    // 3) 焦点探测判 NoInput/Unknown 时根本走不到这里（deliver 提前回退到复制按钮）；
    //    能走到 = 确认输入态或未授权例外，注入落空的风险由剪贴板常驻兜底。
    // 代价：每次听写覆盖剪贴板。对听写工具而言"最后一次结果随手可粘"本就是合理语义。
    if let Err(e) = cb.set_text(text.to_string()) {
        return InjectOutcome::Failed(format!("写剪贴板失败：{e}"));
    }

    // 模拟 Cmd+V（面板 focusable:false，焦点仍在用户原来的应用里）
    if let Err(e) = press_cmd_v() {
        return InjectOutcome::Failed(format!("模拟按键失败：{e}"));
    }

    InjectOutcome::Injected
}

pub fn type_text(text: &str) -> InjectOutcome {
    if !accessibility_ok() {
        return InjectOutcome::NeedPermission;
    }
    // 键入没有回执：事件没人消费（无文本焦点）时文字凭空消失。先在剪贴板留一份兜底，
    // 与 paste 模式语义对齐——最后一次听写结果永远可以在剪贴板找到。
    // 写失败必须留痕（面板承诺"文本在剪贴板"，不能无声说谎）
    if let Err(e) = copy_to_clipboard(text) {
        eprintln!("[inject] 键入模式写剪贴板兜底失败：{e}");
    }
    use enigo::{Enigo, Keyboard, Settings};
    let mut enigo = match Enigo::new(&Settings::default()) {
        Ok(e) => e,
        Err(e) => return InjectOutcome::Failed(format!("{e}")),
    };
    if let Err(e) = enigo.text(text) {
        return InjectOutcome::Failed(format!("{e}"));
    }
    InjectOutcome::Injected
}

// Cmd+V = 按住 Meta(Command) -> 点按 V -> 松开
// （Unicode('v') 会按当前键盘布局映射到 V 键的键码）
fn press_cmd_v() -> Result<(), String> {
    use enigo::{Direction, Enigo, Key, Keyboard, Settings};
    let mut e = Enigo::new(&Settings::default()).map_err(|err| err.to_string())?;
    e.key(Key::Meta, Direction::Press).map_err(|err| err.to_string())?;
    e.key(Key::Unicode('v'), Direction::Click).map_err(|err| err.to_string())?;
    e.key(Key::Meta, Direction::Release).map_err(|err| err.to_string())?;
    Ok(())
}

// 面板"复制"按钮的后盾：只写剪贴板，不碰键盘
pub fn copy_to_clipboard(text: &str) -> Result<(), String> {
    let mut cb = arboard::Clipboard::new().map_err(|e| e.to_string())?;
    cb.set_text(text).map_err(|e| e.to_string())
}
