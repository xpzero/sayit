// 文字注入模块：把识别文字送进"当前光标所在的应用"
// 主方案 = 写剪贴板 + 模拟 Cmd+V（唯一允许写剪贴板的路径之一——⌘V 的机制要求；
// 剪贴板最小侵入政策见 AGENTS.md：非注入路径不写、复制按钮点击才写）
// 备选   = 直接键入：合成携带 Unicode 文本的输入事件（落空兜底走听写历史）
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

// 带系统提示窗的信任查询（2026-11-11 勘误启用）：kAXTrustedCheckOptionPrompt=true 时，
// 未授权则弹**系统级**授权请求窗（「Sayit」想控制此电脑 + 拒绝/打开系统设置，
// 系统流程会把 App 加进辅助功能列表——比自制对话框更原生，带路按钮也是系统的）。
// 返回当前信任状态；弹窗异步展示不阻塞。勘误背景：AGENTS 曾记“辅助功能系统不弹窗”，
// 实为手动引导是兑底、系统提示窗一直存在（用户指出，axuielement 文档在案）
#[cfg(target_os = "macos")]
#[repr(C)]
struct CFDictCallBacks {
    _priv: [usize; 6], // CFDictionaryKeyCallBacks/ValueCallBacks 的不透明布局（6 个指针）
}

#[cfg(target_os = "macos")]
#[link(name = "ApplicationServices", kind = "framework")]
unsafe extern "C" {
    fn AXIsProcessTrustedWithOptions(options: *const core::ffi::c_void) -> bool;
    static kAXTrustedCheckOptionPrompt: *const core::ffi::c_void;
}

#[cfg(target_os = "macos")]
#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    fn CFDictionaryCreate(
        allocator: *const core::ffi::c_void,
        keys: *const *const core::ffi::c_void,
        values: *const *const core::ffi::c_void,
        num_values: isize,
        key_callbacks: *const CFDictCallBacks,
        value_callbacks: *const CFDictCallBacks,
    ) -> *const core::ffi::c_void;
    static kCFBooleanTrue: *const core::ffi::c_void;
    static kCFTypeDictionaryKeyCallBacks: CFDictCallBacks;
    static kCFTypeDictionaryValueCallBacks: CFDictCallBacks;
}

#[cfg(target_os = "macos")]
pub fn ax_trusted_with_prompt() -> bool {
    unsafe {
        let keys = [kAXTrustedCheckOptionPrompt as *const core::ffi::c_void];
        let values = [kCFBooleanTrue as *const core::ffi::c_void];
        let options = CFDictionaryCreate(
            std::ptr::null(),
            keys.as_ptr(),
            values.as_ptr(),
            1,
            &kCFTypeDictionaryKeyCallBacks,
            &kCFTypeDictionaryValueCallBacks,
        );
        AXIsProcessTrustedWithOptions(options)
    }
}

#[cfg(not(target_os = "macos"))]
pub fn ax_trusted_with_prompt() -> bool {
    true
}

/// 系统辅助功能授权弹窗是否在屏：弹窗由独立小进程 universalaccessauthalert 渲染
/// （AXIsProcessTrustedWithOptions 的 prompt，或系统对"申请过又撤销"App 的自动重现）。
/// 它在跑 = 弹窗在屏（或刚被关、进程短暂存活）。libproc 枚举进程名前缀匹配——
/// p_comm 截断到 16 字节，"universalaccessauthalert" 只剩 "universalaccessa"
#[cfg(target_os = "macos")]
pub fn ax_prompt_showing() -> bool {
    // proc_listallpids/proc_name 在 libSystem（无独立 libproc tbd，#[link] 反而链接失败）
    unsafe extern "C" {
        fn proc_listallpids(buffer: *mut i32, size: i32) -> i32;
        fn proc_name(pid: i32, buffer: *mut core::ffi::c_void, buffer_size: u32) -> i32;
    }
    unsafe {
        let mut pids = [0i32; 512];
        let n = proc_listallpids(pids.as_mut_ptr(), (pids.len() * 4) as i32);
        if n <= 0 {
            return false;
        }
        for &pid in &pids[..(n as usize).min(pids.len())] {
            if pid <= 0 {
                continue;
            }
            let mut buf = [0u8; 256];
            if proc_name(pid, buf.as_mut_ptr().cast(), 256) > 0 {
                if let Ok(name) = std::ffi::CStr::from_bytes_until_nul(&buf) {
                    if name.to_bytes().starts_with(b"universalaccess") {
                        return true;
                    }
                }
            }
        }
    }
    false
}

#[cfg(not(target_os = "macos"))]
pub fn ax_prompt_showing() -> bool {
    false
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

// 剪贴板写入代数：任何经我们代码的写入（注入 / 显式复制 / 还原）都递增。
// 还原线程在 sleep 后检查代数——如果有人（用户按复制按钮 / 第二次注入）在我们
// 之后写入了剪贴板，代数已变，放弃还原（防止回滚用户的新内容，六轮审查 D2/D4/D6）。
static CLIPBOARD_GEN: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

pub fn paste_text(text: &str) -> InjectOutcome {
    if !accessibility_ok() {
        return InjectOutcome::NeedPermission;
    }
    let mut cb = match arboard::Clipboard::new() {
        Ok(c) => c,
        Err(e) => return InjectOutcome::Failed(format!("打不开剪贴板：{e}")),
    };

    // paste 模式写剪贴板 = ⌘V 的机制要求。备份 → 注入 → 延迟还原（用户要求恢复此行为，
    // 2026-11-10 曾因竞态移除——还原太早目标 App 还没读完，粘到旧内容。现三重保护：
    // ① 500ms 延迟覆盖 Electron 慢事件循环；② 代数计数器——还原前发现有人（复制按钮/
    // 下一轮注入）写过剪贴板则放弃；③ 文本比对——只在剪贴板仍是我们写入的文本时还原）。
    // 限制：仅备份/还原纯文本；图片/富文本（RTF 格式信息）不还原，我们的文字留作兜底。
    let backup = cb.get_text().ok();
    let my_gen = CLIPBOARD_GEN.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;

    if let Err(e) = cb.set_text(text.to_string()) {
        return InjectOutcome::Failed(format!("写剪贴板失败：{e}"));
    }

    // 模拟 Cmd+V（面板 focusable:false，焦点仍在用户原来的应用里）
    if let Err(e) = press_cmd_v() {
        return InjectOutcome::Failed(format!("模拟按键失败：{e}"));
        // 注意：此处剪贴板已写入但无还原线程——听写文字留在剪贴板恰好是注入失败的
        // 合理兜底（面板亮复制按钮，用户可手动 ⌘V），不做还原
    }

    // 延迟还原：独立线程，不拖主线程
    let text_marker = text.to_string();
    std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(500));
        // 三重检查之一：代数已变 = 有人在我们之后写过剪贴板（用户点复制按钮 / 新一轮注入），放弃还原
        if CLIPBOARD_GEN.load(std::sync::atomic::Ordering::SeqCst) != my_gen {
            return;
        }
        if let Ok(mut cb) = arboard::Clipboard::new() {
            // 三重检查之二：文本比对——只在剪贴板仍是我们写入的文本时还原
            if let Ok(cur) = cb.get_text() {
                if cur == text_marker {
                    if let Some(b) = &backup {
                        let _ = cb.set_text(b.clone());
                        // 三重检查之三：还原本身也递增代数，通知后续可能的还原线程
                        CLIPBOARD_GEN.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    }
                    // 原剪贴板为空或非文本——留我们的文字作兜底（比清空好）
                }
            }
        }
    });

    InjectOutcome::Injected
}

pub fn type_text(text: &str) -> InjectOutcome {
    if !accessibility_ok() {
        return InjectOutcome::NeedPermission;
    }
    // 键入没有回执：事件没人消费（无文本焦点）时文字凭空消失——落空的兜底走
    // 听写历史（设置→历史，逐条显式复制；剪贴板最小侵入：不再偷偷写）
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

// 面板"复制"按钮的后盾：只写剪贴板，不碰键盘。
// 递增代数以通知任何挂起的还原线程"用户显式写入了剪贴板，别还原"
pub fn copy_to_clipboard(text: &str) -> Result<(), String> {
    CLIPBOARD_GEN.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    let mut cb = arboard::Clipboard::new().map_err(|e| e.to_string())?;
    cb.set_text(text).map_err(|e| e.to_string())
}
