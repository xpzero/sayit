pub mod tty;

// 输入焦点探测：交付时刻"光标处有没有文本框"的分层判定链。
//
// 链条：① AX（权威）→ ② 前台 App 黑名单 → ③ 通用终端判定（宿主 TTY）→ ④ Unknown。
// 注入契约（终稿 E，2026-11-10）：有输入态→注入；
// 确认非输入态→按钮；Unknown→乐观注入（调用方 commands.rs deliver 执行，
// 辅助功能未授权时例外放行走注入带路）。误判两个方向都可恢复：
// 误拦 = 手动 ⌘V（文字在剪贴板）；误注入 = 落空（文字同样在剪贴板 + 复制按钮）。
//
// AX 错误码（HIServices AXError.h，SDK 头文件实测核对 2026-11-09）：
//   0 成功 | -25204 CannotComplete（目标 App 忙/消息失败）| -25205 AttributeUnsupported
//   -25211 APIDisabled（辅助功能被拒）| -25212 NoValue（"该属性没有值"）
// 历史勘误：早期记录把 -25212 当成 APIDisabled——真身是 NoValue，APIDisabled 是 -25211。
// 故第一步"systemwide 查 AXFocusedApplication"承担通道体检：健康通道必有前台 App，
// 出现 -25211/-25212 都按"通道不可用"处理（Unknown，走黑名单兜底）——绝不能把通道
// 错误当成"非输入态"；只有通道确认健康后，editable-ancestor 的 -25212 才是
// "不可编辑"的合法回答（NoValue 语义）。
//
// AX 调用是同步 XPC：目标 App 卡死会拖住调用方。probe() 把实际查询放独立线程并施加
// 整体死线，超时按 Unknown 收场——探测永远不能比交付本身更重要。

/// 判定结果
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// 有可编辑焦点：放心注入
    Input,
    /// 前台无文本框（AX 判定或黑名单命中）：跳过注入，结果位亮复制按钮
    NoInput,
    /// 判不出（通道不可用/目标忙/超时/AX 盲 App）：乐观注入（用户按键即意图），文字不丢在历史里
    /// （用户契约；唯一例外——辅助功能未授权时调用方放行走注入带路，见 commands.rs）
    Unknown,
}

/// 探测报告：判定 + 人话依据（设置页"焦点体检"与日志对账用）
#[derive(Debug, Clone)]
pub struct ProbeReport {
    pub verdict: Verdict,
    pub basis: String,
}

// ---------- AX 访问层：手写 FFI ----------
// 曾评估换 axuielement crate（2026-11-10）：0.8+ 全系带 Swift 桥，在仅 CommandLineTools
// （无完整 Xcode）的机器上链接失败（swiftCompatibility56 符号未定义）——整套开发环境
// 都会踩，弃用。手写 FFI 经三轮独立审查，读路径 + 唤醒全在此。

const AX_SUCCESS: i32 = 0;
const AX_NO_VALUE: i32 = -25212;

const K_CFSTRING_ENCODING_UTF8: u32 = 0x0800_0100;

#[link(name = "ApplicationServices", kind = "framework")]
unsafe extern "C" {
    fn AXUIElementCreateSystemWide() -> *const core::ffi::c_void;
    fn AXUIElementCreateApplication(pid: i32) -> *const core::ffi::c_void;
    fn AXUIElementCopyAttributeValue(
        element: *const core::ffi::c_void,
        attribute: *const core::ffi::c_void, // CFStringRef
        value: *mut *const core::ffi::c_void, // CFTypeRef*（CF_RETURNS_RETAINED）
    ) -> i32;
    // 唤醒路径专用：设 AXManualAccessibility / AXEnhancedUserInterface
    fn AXUIElementSetAttributeValue(
        element: *const core::ffi::c_void,
        attribute: *const core::ffi::c_void,
        value: *const core::ffi::c_void,
    ) -> i32;
    // CoreFoundation（ApplicationServices 是伞形框架，CoreFoundation 随之可用）
    fn CFStringCreateWithCString(
        alloc: *const core::ffi::c_void,
        c_str: *const core::ffi::c_char,
        encoding: u32,
    ) -> *const core::ffi::c_void;
    fn CFStringGetCStringPtr(cf: *const core::ffi::c_void, encoding: u32)
        -> *const core::ffi::c_char;
    fn CFStringGetCString(
        cf: *const core::ffi::c_void,
        buffer: *mut core::ffi::c_char,
        buffer_size: isize,
        encoding: u32,
    ) -> bool;
    fn CFRelease(cf: *const core::ffi::c_void);
    static kCFBooleanTrue: *const core::ffi::c_void;
}

/// RAII 持有 CoreFoundation 对象（Create/Copy 规则：调用方负责释放）
struct CfRef(*const core::ffi::c_void);
impl CfRef {
    fn new(p: *const core::ffi::c_void) -> Self {
        CfRef(p)
    }
    fn is_null(&self) -> bool {
        self.0.is_null()
    }
}
impl Drop for CfRef {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe { CFRelease(self.0) };
        }
    }
}

/// 构造 CFString 属性名（如 "AXFocusedApplication"）
fn cf_str(s: &str) -> Option<CfRef> {
    let c = std::ffi::CString::new(s).ok()?;
    let p = unsafe { CFStringCreateWithCString(std::ptr::null(), c.as_ptr(), K_CFSTRING_ENCODING_UTF8) };
    (!p.is_null()).then_some(CfRef::new(p))
}

/// CFString → Rust String（先试零拷贝指针，失败退回缓冲区拷贝）
fn cf_to_string(cf: &CfRef) -> Option<String> {
    unsafe {
        let p = CFStringGetCStringPtr(cf.0, K_CFSTRING_ENCODING_UTF8);
        if !p.is_null() {
            return std::ffi::CStr::from_ptr(p).to_string_lossy().into_owned().into();
        }
        let mut buf = vec![0i8; 1024];
        if CFStringGetCString(cf.0, buf.as_mut_ptr(), buf.len() as isize, K_CFSTRING_ENCODING_UTF8) {
            let bytes: Vec<u8> = buf.iter().take_while(|&&b| b != 0).map(|&b| b as u8).collect();
            String::from_utf8(bytes).ok()
        } else {
            None
        }
    }
}

fn copy_attr(el: *const core::ffi::c_void, attr: &CfRef) -> (i32, *const core::ffi::c_void) {
    let mut v: *const core::ffi::c_void = std::ptr::null();
    let err = unsafe { AXUIElementCopyAttributeValue(el, attr.0, &mut v) };
    (err, v)
}

// ---------- 判定核心（纯函数，语义全部固化在 mod tests） ----------

/// 三步查询的错误码 → 判定。
/// - `app_err`：systemwide 查 AXFocusedApplication 的结果——这一步是通道体检
/// - `focus_err`：前台 App 查 AXFocusedUIElement（None = 因前置失败未查询）
/// - `editable_err`：焦点元素查 AXEditableAncestor（None = 未查询）
/// - `role`：焦点元素的 AXRole（editable-ancestor 不被支持时的退路）
pub fn classify(app_err: i32, focus_err: Option<i32>, editable_err: Option<i32>, role: Option<&str>) -> Verdict {
    use Verdict::*;
    // 通道体检：健康通道必有前台 App。任何错误（含 -25212/-25211）都不下结论，
    // 交给黑名单兜底——绝不能把通道故障冒充"非输入态"
    if app_err != AX_SUCCESS {
        return Unknown;
    }
    let Some(fe) = focus_err else { return Unknown };
    // 通道健康。前台 App 连焦点元素都没有 = 无处落字
    if fe != AX_SUCCESS {
        return match fe {
            AX_NO_VALUE => NoInput,
            _ => Unknown, // 目标忙/属性不支持等：不下结论
        };
    }
    match editable_err {
        Some(e) if e == AX_SUCCESS => Input,    // 有可编辑祖先（含焦点元素自身）
        Some(e) if e == AX_NO_VALUE => NoInput, // 焦点元素不可编辑（NoValue 的合法语义）
        // editable-ancestor 不被支持（-25205 等）：退回标准文本角色表
        Some(_) => match role {
            Some("AXTextField") | Some("AXTextArea") | Some("AXSecureTextField") => Input,
            _ => Unknown,
        },
        None => Unknown,
    }
}

// ---------- App 名单兜底（只裁决 Unknown；AX 的 Input/NoInput 是权威，名单不改写） ----------

/// 确定永远没有文本焦点的系统 App（AX 通道不可用时唯一能给的答案）。
/// 取舍：Finder 的搜索/重命名框也会命中黑名单——手动 ⌘V 即可，文字不丢。
const NO_INPUT_APPS: &[&str] = &[
    "com.apple.finder",
    "com.apple.dock",
    "com.apple.systemuiserver",
    "com.apple.wallpaper",
];

pub fn blacklist_hit(bundle_id: &str) -> bool {
    NO_INPUT_APPS.iter().any(|b| bundle_id.eq_ignore_ascii_case(b))
}

pub fn probe(deadline: std::time::Duration) -> ProbeReport {
    // 前台 App：名单/TTY 兜底的判据 + 观测点（排查"为什么没亮复制按钮"的第一现场）
    let frontmost = crate::inject::frontmost_app();
    let pid = frontmost.as_ref().map(|(_, p)| *p);
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(ax_probe(pid));
    });
    let report = match rx.recv_timeout(deadline) {
        Ok(r) => r,
        Err(_) => ProbeReport {
            verdict: Verdict::Unknown,
            basis: "AX 探测超时（目标 App 无响应）".into(),
        },
    };
    let report = if report.verdict == Verdict::Unknown {
        let tty_host = frontmost.as_ref().map(|(_, pid)| tty::hosts_tty(*pid)).unwrap_or(false);
        with_app_lists(report, frontmost.as_ref().map(|(b, _)| b.as_str()), tty_host)
    } else {
        report
    };
    println!("[focus] 判定 {:?}（前台 {:?}）：{}", report.verdict, frontmost, report.basis);
    debug_log(&report, &frontmost);
    report
}

/// 真实的 AX 探测：常规三步链；全灭且是已知前台 pid 时拨开 Electron/Chromium 的
/// AX 开关后重试。这两类应用的 AX 树默认关闭（查询全灭/-25212）——设
/// AXManualAccessibility（Electron 认）/ AXEnhancedUserInterface（Chromium 浏览器认）
/// 为 true 后树会异步建起来，之后逐状态判定可用（Electron 官方文档记载的第三方机制）。
/// 首轮可能赶不上调用方死线（返回 Unknown 走乐观注入），开关已持久、下一轮就快了。
fn ax_probe(pid: Option<i32>) -> ProbeReport {
    let report = ax_query_chain();
    if report.verdict != Verdict::Unknown {
        return report;
    }
    let Some(pid) = pid else { return report };
    if !wake_ax(pid) {
        return report;
    }
    // 建树是异步的：小睡重试几拍（超出调用方死线也无妨——线程自嗨，不拖交付）
    for _ in 0..3 {
        std::thread::sleep(std::time::Duration::from_millis(150));
        let r = ax_query_chain();
        if r.verdict != Verdict::Unknown {
            let mut basis = r.basis;
            basis.push_str("；AX 唤醒后成功");
            return ProbeReport { verdict: r.verdict, basis };
        }
    }
    let mut basis = report.basis;
    basis.push_str("；已拨 AX 开关，树未及建好（下一轮应可查）");
    ProbeReport { verdict: Verdict::Unknown, basis }
}

/// 拨 Chromium 系的 AX 开关。两个属性都设（Electron 认 Manual、Chrome 认 Enhanced——
/// 文档实证，见计划书来源）；部分 Electron 版本可能拒绝 set（已知 issue），优雅降级。
fn wake_ax(pid: i32) -> bool {
    unsafe {
        let app = CfRef::new(AXUIElementCreateApplication(pid));
        if app.is_null() {
            return false;
        }
        let Some(a_manual) = cf_str("AXManualAccessibility") else { return false };
        let Some(a_enhanced) = cf_str("AXEnhancedUserInterface") else { return false };
        let manual = AXUIElementSetAttributeValue(app.0, a_manual.0, kCFBooleanTrue) == AX_SUCCESS;
        let enhanced =
            AXUIElementSetAttributeValue(app.0, a_enhanced.0, kCFBooleanTrue) == AX_SUCCESS;
        println!("[focus] AX 开关 pid={pid}：Manual={manual} Enhanced={enhanced}");
        manual || enhanced
    }
}

/// 三步查询（systemwide → focusedApplication → focusedUIElement → editableAncestor）
fn ax_query_chain() -> ProbeReport {
    unsafe {
        let sys = CfRef::new(AXUIElementCreateSystemWide());
        if sys.is_null() {
            return unknown("systemwide 元素创建失败");
        }
        let Some(a_app) = cf_str("AXFocusedApplication") else {
            return unknown("CFString 构造失败");
        };
        let Some(a_focus) = cf_str("AXFocusedUIElement") else {
            return unknown("CFString 构造失败");
        };
        let Some(a_edit) = cf_str("AXEditableAncestor") else {
            return unknown("CFString 构造失败");
        };
        let Some(a_role) = cf_str("AXRole") else {
            return unknown("CFString 构造失败");
        };

        // ① 通道体检：前台 App 元素
        let (app_err, app) = copy_attr(sys.0, &a_app);
        let app = CfRef::new(app);

        // ② 前台 App 的焦点元素
        let (focus_err, focused) = if app.is_null() {
            (None, CfRef::new(std::ptr::null()))
        } else {
            let (e, v) = copy_attr(app.0, &a_focus);
            (Some(e), CfRef::new(v))
        };

        // ③ 焦点元素的可编辑祖先；仅在需要退路时才多花一跳查 role
        let (editable_err, role) = if focused.is_null() {
            (None, None)
        } else {
            let (e, v) = copy_attr(focused.0, &a_edit);
            let _anc = CfRef::new(v); // CF_RETURNS_RETAINED：成功路径也有 +1 引用，必须接住释放
            let role = if e != AX_SUCCESS && e != AX_NO_VALUE {
                let (re, rv) = copy_attr(focused.0, &a_role);
                // SUCCESS 但值为 null 属病态（违反 API 契约），按拿不到处理——防 null 进 FFI
                if re == AX_SUCCESS && !rv.is_null() {
                    cf_to_string(&CfRef::new(rv))
                } else {
                    None
                }
            } else {
                None
            };
            (Some(e), role)
        };

        let verdict = classify(app_err, focus_err, editable_err, role.as_deref());
        ProbeReport {
            verdict,
            basis: format!(
                "AX（app={app_err}, focus={focus_err:?}, editable={editable_err:?}, role={role:?}）"
            ),
        }
    }
}

fn unknown(basis: &str) -> ProbeReport {
    ProbeReport { verdict: Verdict::Unknown, basis: basis.into() }
}

/// 名单/TTY 兜底：Unknown 时按前台 App 裁决——黑名单 → NoInput；宿主 TTY（终端类，
/// 通用判定）→ Input；都不是 → 维持 Unknown（调用方按乐观注入处理）。
/// 纯函数（TTY 判定结果由调用方算好传入），断言见 tests。
fn with_app_lists(mut report: ProbeReport, frontmost: Option<&str>, tty_host: bool) -> ProbeReport {
    if report.verdict == Verdict::Unknown {
        if let Some(bid) = frontmost {
            if blacklist_hit(bid) {
                report.basis = format!("{}；前台黑名单命中 {bid}", report.basis);
                report.verdict = Verdict::NoInput;
            } else if tty_host {
                report.basis = format!("{}；前台宿主命令行（终端判定）{bid}", report.basis);
                report.verdict = Verdict::Input;
            }
        }
    }
    report
}

// 调试落盘（排查授权/探测链时唯一能被外部直接读取的观测点；无 TCC 门槛的 /tmp 路径）。
// 每行带时间戳 + AXIsProcessTrusted 读数（它会说谎——与真实错误码并列正好取证）。
// 超 1MB 自动轮转（重开）：取证日志不该无限长（三轮审查 D6）。
fn debug_log(report: &ProbeReport, frontmost: &Option<(String, i32)>) {
    use std::io::Write;
    const LOG: &str = "/tmp/sayit-focus.log";
    if std::fs::metadata(LOG).map(|m| m.len() > 1_000_000).unwrap_or(false) {
        let _ = std::fs::remove_file(LOG);
    }
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let line = format!(
        "[{ts}] 判定 {:?}（前台 {:?}）axTrusted={} ：{}\n",
        report.verdict,
        frontmost,
        crate::inject::accessibility_ok(),
        report.basis
    );
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(LOG)
    {
        let _ = f.write_all(line.as_bytes());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    // 通道级错误码（完整语义表见文件头；测试自文档用）
    const AX_CANNOT_COMPLETE: i32 = -25204;
    const AX_ATTRIBUTE_UNSUPPORTED: i32 = -25205;
    const AX_API_DISABLED: i32 = -25211;

    #[test]
    fn 分类_通道级错误一律unknown不冒充非输入态() {
        // 第一步查询（focusedApplication）失败 = 通道不可用。历史事故里 -25212/-25211
        // 都在这层出现过，无论哪个码、无论后面还有什么，都不能下 NoInput 结论
        assert_eq!(classify(AX_NO_VALUE, None, None, None), Verdict::Unknown);
        assert_eq!(classify(AX_API_DISABLED, None, None, None), Verdict::Unknown);
        assert_eq!(classify(AX_CANNOT_COMPLETE, None, None, None), Verdict::Unknown);
        assert_eq!(classify(-25200, Some(AX_SUCCESS), Some(AX_SUCCESS), None), Verdict::Unknown);
    }

    #[test]
    fn 分类_通道健康后按焦点与可编辑祖先判定() {
        assert_eq!(classify(AX_SUCCESS, Some(AX_SUCCESS), Some(AX_SUCCESS), None), Verdict::Input);
        assert_eq!(classify(AX_SUCCESS, Some(AX_SUCCESS), Some(AX_NO_VALUE), None), Verdict::NoInput);
        // 前台 App 没有焦点元素 = 无处落字
        assert_eq!(classify(AX_SUCCESS, Some(AX_NO_VALUE), None, None), Verdict::NoInput);
        // 目标忙：不下结论
        assert_eq!(classify(AX_SUCCESS, Some(AX_CANNOT_COMPLETE), None, None), Verdict::Unknown);
        assert_eq!(classify(AX_SUCCESS, Some(AX_SUCCESS), Some(AX_CANNOT_COMPLETE), None), Verdict::Unknown);
        assert_eq!(classify(AX_SUCCESS, Some(AX_SUCCESS), None, None), Verdict::Unknown);
    }

    #[test]
    fn 分类_可编辑祖先不支持时退回角色表() {
        let e = Some(AX_ATTRIBUTE_UNSUPPORTED);
        assert_eq!(classify(AX_SUCCESS, Some(AX_SUCCESS), e, Some("AXTextArea")), Verdict::Input);
        assert_eq!(classify(AX_SUCCESS, Some(AX_SUCCESS), e, Some("AXTextField")), Verdict::Input);
        assert_eq!(classify(AX_SUCCESS, Some(AX_SUCCESS), e, Some("AXSecureTextField")), Verdict::Input);
        assert_eq!(classify(AX_SUCCESS, Some(AX_SUCCESS), e, Some("AXButton")), Verdict::Unknown);
        assert_eq!(classify(AX_SUCCESS, Some(AX_SUCCESS), e, None), Verdict::Unknown);
    }

    #[test]
    fn 黑名单_覆盖已知无输入系统App_大小写不敏感() {
        assert!(blacklist_hit("com.apple.finder"));
        assert!(blacklist_hit("com.apple.dock"));
        assert!(blacklist_hit("com.apple.wallpaper"));
        assert!(blacklist_hit("COM.APPLE.SYSTEMUISERVER"));
        assert!(!blacklist_hit("com.apple.TextEdit"));
        assert!(!blacklist_hit("com.tencent.xinWeChat"));
        assert!(!blacklist_hit(""));
    }

    #[test]
    fn 名单裁决_黑名单降级_tty升级_只在unknown时生效() {
        let r = |v: Verdict| ProbeReport { verdict: v, basis: "b".into() };
        // 黑名单 → NoInput
        assert_eq!(with_app_lists(r(Verdict::Unknown), Some("com.apple.finder"), false).verdict, Verdict::NoInput);
        // 宿主 TTY（终端通用判定）→ Input：wezterm 实测 AX 只报 AXWindow（Unknown），注入该照常
        assert_eq!(with_app_lists(r(Verdict::Unknown), Some("com.github.wez.wezterm"), true).verdict, Verdict::Input);
        // 无 TTY 且不在黑名单（如 AX 盲的 Electron 聊天 App）→ 维持 Unknown
        assert_eq!(with_app_lists(r(Verdict::Unknown), Some("com.deepseek.dsh"), false).verdict, Verdict::Unknown);
        assert_eq!(with_app_lists(r(Verdict::Unknown), None, true).verdict, Verdict::Unknown);
        // 已有判定不被改写（AX 的 Input/NoInput 是权威，兜底只补 Unknown 的窟窿）
        assert_eq!(with_app_lists(r(Verdict::Input), Some("com.apple.finder"), true).verdict, Verdict::Input);
        assert_eq!(with_app_lists(r(Verdict::NoInput), Some("com.github.wez.wezterm"), true).verdict, Verdict::NoInput);
        // 裁决依据写进 basis（观测点可查）
        assert!(with_app_lists(r(Verdict::Unknown), Some("com.apple.finder"), false).basis.contains("黑名单命中"));
        assert!(with_app_lists(r(Verdict::Unknown), Some("x"), true).basis.contains("宿主命令行"));
    }

}
