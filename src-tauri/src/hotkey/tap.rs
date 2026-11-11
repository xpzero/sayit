// CGEventTap：全局按键事件的统一入口（主动会话 tap：可吞键）
// 取代 Carbon 全局快捷键注册与 20ms 轮询，一套逻辑处理：
//   - 组合触发：keyDown 的 (flags, keycode) 与匹配集比对
//   - Fn 触发/录入：flagsChanged（keycode 63 + SecondaryFn 位）
//   - Esc 中止录音、捕获模式录制（keyDown 预览 / keyUp 提交，自动重复字段精确去重）
// 吞键政策（2026-11-10 用户拍板"绑定即独占"）：**只吞已注册组合**（听写/粘贴键的精确
// 修饰符+键码命中，按下与配套抬起），返回 NULL = 事件不再到达前台应用与下游监听者。
// 其余一律放行：Esc 中止（属当前上下文）、录制期一切按键、未注册键。默认键 ⌥Space
// 视同已注册（否则双重动作依旧）。注意：tap 失效（被系统禁用）时键会漏给应用——
// 有自愈（TAP_DISABLED_BY_*），漏键是安全方向的降级。
// 前提：进程需有辅助功能权限才能收到 keyDown（应用本就要它做文字注入）；
// flagsChanged 无需权限。Menu 键(110)实测不派发事件，由 fn_listener 的状态轮询兜底。
// shortcut: 无看门狗定时器——tap 回调卡死 >1s 才会被系统禁用，实践中不会发生；若发生重启应用自愈
use std::sync::atomic::{AtomicBool, AtomicU16, Ordering};
use std::sync::Mutex;
use tauri::{AppHandle, Emitter};

use super::should_handle;

unsafe extern "C" {
    // CoreFoundation
    fn CFMachPortCreateRunLoopSource(
        allocator: *const core::ffi::c_void,
        port: *const core::ffi::c_void,
        order: isize,
    ) -> *const core::ffi::c_void;
    fn CFRunLoopGetCurrent() -> *const core::ffi::c_void;
    fn CFRunLoopAddSource(
        rl: *const core::ffi::c_void,
        source: *const core::ffi::c_void,
        mode: *const core::ffi::c_void,
    );
    fn CFRunLoopRun();
    static kCFRunLoopCommonModes: *const core::ffi::c_void;
    // CoreGraphics
    fn CGEventTapCreate(
        tap: i32,
        place: i32,
        options: isize,
        event_mask: u64,
        callback: extern "C" fn(
            proxy: *const core::ffi::c_void,
            etype: u32,
            event: *const core::ffi::c_void,
            user_info: *const core::ffi::c_void,
        ) -> *const core::ffi::c_void,
        user_info: *const core::ffi::c_void,
    ) -> *const core::ffi::c_void;
    fn CGEventGetIntegerValueField(event: *const core::ffi::c_void, field: isize) -> i64;
    fn CGEventGetFlags(event: *const core::ffi::c_void) -> u64;
    fn CGEventTapEnable(tap: *const core::ffi::c_void, enable: bool);
    fn CGEventGetLocation(event: *const core::ffi::c_void) -> CGPoint;
    fn CFRunLoopStop(rl: *const core::ffi::c_void);
    fn CFRunLoopRemoveSource(
        rl: *const core::ffi::c_void,
        source: *const core::ffi::c_void,
        mode: *const core::ffi::c_void,
    );
    fn CFMachPortInvalidate(port: *const core::ffi::c_void);
    fn CFRelease(cf: *const core::ffi::c_void);
}

const SESSION_TAP: i32 = 1; // kCGSessionEventTap
const HEAD_INSERT: i32 = 0; // kCGHeadInsertEventTap
const ACTIVE_TAP: isize = 0; // kCGEventTapOptionDefault：主动 tap——回调返回 NULL 可吞键（独占捕获的前提）
// CGEvent 的事件类型编号（CGEventTypes.h）：keyDown=10、keyUp=11、flagsChanged=12。
// 曾误写成 1/2/12——1/2 其实是左键按下/抬起（鼠标事件！）：tap 只看得见鼠标点击和 Fn，
// 字母/数字/空格组合键与 Esc 中止全部失灵；鼠标点击还顶着"键码 0"的马甲混进
// keyDown 通道（即 handle_key_down 里那只"幽灵键码 0"的真身）
const MASK: u64 = (1 << 10) | (1 << 11) | (1 << 12) | (1 << 1); // keyDown | keyUp | flagsChanged | leftMouseDown（仅观察：面板提示的点外收板用，不吞）
const TAP_DISABLED_BY_TIMEOUT: u32 = 0xFFFF_FFE1; // kCGEventTapDisabledByTimeout
const TAP_DISABLED_BY_USER: u32 = 0xFFFF_FFE0; // kCGEventTapDisabledByUserInput

const T_KEYDOWN: u32 = 10;
const T_KEYUP: u32 = 11;
const T_FLAGS_CHANGED: u32 = 12;
const T_LEFT_MOUSE_DOWN: u32 = 1;

#[repr(C)]
struct CGPoint {
    x: f64,
    y: f64,
}

const F_KEYCODE: isize = 9; // kCGKeyboardEventKeycode
const F_AUTOREPEAT: isize = 8; // kCGKeyboardEventAutorepeat

pub const MOD_SHIFT: u64 = 1 << 17;
pub const MOD_CTRL: u64 = 1 << 18;
pub const MOD_OPTION: u64 = 1 << 19;
pub const MOD_COMMAND: u64 = 1 << 20;
const MOD_ALL: u64 = MOD_SHIFT | MOD_CTRL | MOD_OPTION | MOD_COMMAND;
const FN_FLAG: u64 = 1 << 23;

pub const K_ESCAPE: u16 = 53;
pub const K_BACKSPACE: u16 = 51;
const K_FN: i64 = 63; // flagsChanged 事件里 Fn 自带的键码
pub const K_CTX_MENU: u16 = 110; // 探针实测：状态可见但无事件（fn_listener 轮询兜底）

// 匹配集：编译后的 (修饰符位, 键码)。register_all 全量热替换，无变化时是空操作。
static DICTATION: Mutex<Vec<(u64, u16)>> = Mutex::new(Vec::new());
static FN_ON: AtomicBool = AtomicBool::new(false);
static CAPTURING: AtomicBool = AtomicBool::new(false);
// 捕获辅助态：待 keyUp 提交的组合 + "Fn 按住期间夹过主键"标记
static PENDING: AtomicU16 = AtomicU16::new(0);
static PENDING_COMBO: Mutex<Option<String>> = Mutex::new(None);
static MAIN_SEEN: AtomicBool = AtomicBool::new(false);
static TAP_PORT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
// 当前 tap 线程的 runloop（撤销看门狗停它用；0 = tap 未运行）
static TAP_RUNLOOP: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

pub fn set_dictation(combos: Vec<(u64, u16)>, fn_on: bool) {
    if let Ok(mut set) = DICTATION.lock() {
        *set = combos;
    }
    FN_ON.store(fn_on, Ordering::Relaxed);
}

pub fn start_capture() {
    CAPTURING.store(true, Ordering::Relaxed);
}

pub fn stop_capture() {
    CAPTURING.store(false, Ordering::Relaxed);
}

pub fn capturing() -> bool {
    CAPTURING.load(Ordering::Relaxed)
}

// 键码 -> 组合键方言名（KeyA/Digit1 剥前缀成短名，与配置方言一致）
fn combo_name(mods: u64, keycode: u16) -> String {
    let mut parts: Vec<&str> = Vec::new();
    if mods & MOD_COMMAND != 0 {
        parts.push("Command");
    }
    if mods & MOD_CTRL != 0 {
        parts.push("Ctrl");
    }
    if mods & MOD_OPTION != 0 {
        parts.push("Option");
    }
    if mods & MOD_SHIFT != 0 {
        parts.push("Shift");
    }
    let main = super::key_name(keycode)
        .map(|n| n.trim_start_matches("Key").trim_start_matches("Digit"))
        .unwrap_or("?");
    parts.push(main);
    parts.join("+")
}

fn is_modifier_keycode(kc: u16) -> bool {
    (54..=63).contains(&kc)
}

// 热键动作必须回主线程执行：注入走 enigo→HIToolbox 输入法服务，
// 那是 main-thread-only API，非主线程调用会被 dispatch_assert_queue 直接 trap（实测崩溃）
fn dispatch_toggle(app: &AppHandle) {
    let inner = app.clone();
    let _ = app.run_on_main_thread(move || crate::commands::toggle_recording_impl(&inner));
}

fn dispatch_abort(app: &AppHandle) {
    let inner = app.clone();
    let _ = app.run_on_main_thread(move || crate::commands::abort_recording_impl(&inner));
}

// 回调不可 panic（跨 FFI = UB），所有分支只做匹配与 emit。
// 主动 tap（可吞键）：返回 NULL = 事件从流中移除（前台 App 与下游监听者都收不到）——
// 已注册快捷键（听写/粘贴）按下与抬起均吞（绑定即全局独占）；Esc 中止与录制期不吞。
extern "C" fn on_event(
    _proxy: *const core::ffi::c_void,
    etype: u32,
    event: *const core::ffi::c_void,
    user_info: *const core::ffi::c_void,
) -> *const core::ffi::c_void {
    let app = user_info as *const AppHandle;
    let Some(app) = (unsafe { app.as_ref() }) else {
        return event;
    };
    let keycode = unsafe { CGEventGetIntegerValueField(event, F_KEYCODE) };
    let flags = unsafe { CGEventGetFlags(event) };

    // tap 被系统禁用（回调曾超时/被用户输入禁用）时的自愈：标准 CFEventTap 模式
    if etype == TAP_DISABLED_BY_TIMEOUT || etype == TAP_DISABLED_BY_USER {
        let port = TAP_PORT.load(Ordering::Relaxed) as *const core::ffi::c_void;
        if !port.is_null() {
            unsafe { CGEventTapEnable(port, true) };
        }
        return event;
    }

    match etype {
        T_LEFT_MOUSE_DOWN => {
            // 仅观察不吞：点击照常到达用户的应用。点在面板外且面板可见且空闲 → 收板
            // （提示胶囊的"点空白即收"；坐标与 PANEL_FRAME 同为 CG 系逻辑点）
            let p = unsafe { CGEventGetLocation(event) };
            crate::panel::try_click_outside_hide(app, p.x, p.y);
        }
        T_FLAGS_CHANGED => handle_flags_changed(app, keycode, flags),
        T_KEYDOWN => {
            if unsafe { CGEventGetIntegerValueField(event, F_AUTOREPEAT) } != 0 {
                return event; // 自动重复：精确丢弃
            }
            if handle_key_down(app, keycode as u16, flags) {
                return std::ptr::null(); // 吞键
            }
        }
        T_KEYUP => {
            if handle_key_up(app, keycode as u16, flags) {
                return std::ptr::null(); // 配套 keyUp 一起吞（避免孤儿抬起事件漏给 App）
            }
        }
        _ => {}
    }
    event
}

fn handle_flags_changed(app: &AppHandle, keycode: i64, flags: u64) {
    if keycode != K_FN {
        return;
    }
    let fn_down = flags & FN_FLAG != 0;

    if fn_down {
        MAIN_SEEN.store(false, Ordering::Relaxed); // 新手势清零
        if !capturing() && FN_ON.load(Ordering::Relaxed) && should_handle() {
            dispatch_toggle(app);
        }
        return;
    }

    // Fn 抬起：捕获态且全程无主键、无其他修饰符 = 录入 Fn 令牌
    if capturing() && flags & MOD_ALL == 0 && !MAIN_SEEN.load(Ordering::Relaxed) {
        let _ = app.emit("hotkey-captured", serde_json::json!({ "key": "Fn" }));
    }
}

/// keyDown 处理。返回 true = 吞掉该事件（已注册快捷键的独占语义）
fn handle_key_down(app: &AppHandle, kc: u16, flags: u64) -> bool {
    if kc == 0 {
        return false; // 键码 0 防御性丢弃（历史上是鼠标点击混进来的"幽灵键"；见 MASK 注释）
    }
    if is_modifier_keycode(kc) {
        // Fn+修饰键手势不算"Fn 单独按"（否则按 Fn+Shift、先松 Shift 再松 Fn 会误录成裸 Fn）
        if capturing() {
            MAIN_SEEN.store(true, Ordering::Relaxed);
        }
        return false;
    }

    if capturing() {
        match kc {
            K_ESCAPE => {
                stop_capture();
                let _ = app.emit("hotkey-captured", serde_json::json!({ "cancelled": true }));
            }
            K_BACKSPACE => {
                let _ = app.emit("hotkey-remove-last", ());
            }
            K_CTX_MENU => {
                // 若系统真派发 110 的 keyDown（实测多数派发）就直接录入；轮询是兜底
                let _ = app.emit("hotkey-captured", serde_json::json!({ "key": "ContextMenu" }));
            }
            _ if super::key_name(kc).is_some() => {
                MAIN_SEEN.store(true, Ordering::Relaxed); // Fn 组合手势 disarm 标记
                let name = combo_name(flags & MOD_ALL, kc);
                let _ = app.emit("hotkey-preview", serde_json::json!({ "key": name }));
                PENDING.store(kc, Ordering::Relaxed);
                if let Ok(mut g) = PENDING_COMBO.lock() {
                    *g = Some(name);
                }
            }
            _ => {} // 键码表之外的键：丢弃（录进去只会产出注册不了的垃圾条目）
        }
        return false; // 录制期一律放行（用户在录入新键，原样到达输入框）
    }

    // Esc 中止录音（进行中才动）。读无锁相位镜像而非 SharedState——
    // 回调等互斥锁超过 ~1s 会被系统禁用 tap，全部热键失灵。Esc 不吞（属于当前上下文）
    if kc == K_ESCAPE {
        if crate::commands::ACTIVE.load(Ordering::Relaxed) {
            dispatch_abort(app);
        }
        return false;
    }

    // 组合匹配触发。命中即吞——绑定即全局独占，
    // 即便防抖吞掉了动作（双路径竞态）也不该把"我们的键"漏给别的 App
    let combo = (flags & MOD_ALL, kc);
    if registered_combo(combo) {
        if should_handle() {
            dispatch_toggle(app);
        }
        return true;
    }
    false
}

/// keyUp 处理。返回 true = 吞（已注册组合的配套抬起；录制期提交逻辑照旧、不吞）
fn handle_key_up(app: &AppHandle, kc: u16, flags: u64) -> bool {
    if capturing() {
        if kc == 0 || PENDING.load(Ordering::Relaxed) != kc {
            return false;
        }
        PENDING.store(0, Ordering::Relaxed);
        if let Ok(mut g) = PENDING_COMBO.lock() {
            if let Some(name) = g.take() {
                let _ = app.emit("hotkey-captured", serde_json::json!({ "key": name }));
            }
        }
        return false;
    }
    // 非录制期：吞已注册组合的 keyUp（修饰符先松导致 flags 不匹配时漏一个孤儿
    // keyUp，App 一般忽略——不做有状态跟踪，接受这个边角）
    registered_combo((flags & MOD_ALL, kc))
}

/// (修饰符位, 键码) 是否命中注册集（纯判定，断言见 mod tests）
fn registered_combo(combo: (u64, u16)) -> bool {
    DICTATION
        .lock()
        .map(|set| set.contains(&combo))
        .unwrap_or(false)
}

// 裸指针不是 Send，包一层带声明的载体（AppHandle 自身线程安全，指针只作携带）。
// 注意线程体必须是独立函数：Rust 2021 闭包按字段精确捕获，
// 若在闭包里解构 SendPtr 会直接捕获裸指针字段、绕过这里的 unsafe impl Send。
struct SendPtr(*const AppHandle);
unsafe impl Send for SendPtr {}

// tap 侧观测日志（GUI app 的 stderr 不可见——2026-11-11 打包版取证全靠系统日志重建，
// 补文件观测点。超 1MB 轮转，与 sayit-focus.log 同款）
fn tap_log(line: &str) {
    use std::io::Write;
    const LOG: &str = "/tmp/sayit-tap.log";
    if std::fs::metadata(LOG).map(|m| m.len() > 1_000_000).unwrap_or(false) {
        let _ = std::fs::remove_file(LOG);
    }
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(LOG) {
        let _ = writeln!(f, "[{ts}] {line}");
    }
    println!("[tap] {line}");
}

fn run_tap(carrier: SendPtr) {
    unsafe {
        let SendPtr(info) = carrier;
        // 生命周期外环（创建监护重试 + 撤销自愈，2026-11-11）：
        // - 创建重试：首启时序竞态——授权前启动的进程 tap 创建被 TCC 拒绝
        //   （kTCCServiceListenEvent authValue=0），低频轮询（2s）直到授权到位自动重建
        //   （原实现永不重试，用户须手动重启）。
        // - 撤销自愈：主动 tap 的存活系于辅助功能授权——运行中被撤销时系统直接断流
        //   （收不到禁用事件，既有 TAP_DISABLED 自愈无从触发）。看门狗发现授权翻 false
        //   即停 runloop，外环接手：拆旧 tap 回到创建重试，授权回来自动复活。
        // 观测点逐段落 /tmp/sayit-tap.log
        loop {
            let tap = loop {
                let t = CGEventTapCreate(
                    SESSION_TAP,
                    HEAD_INSERT,
                    ACTIVE_TAP,
                    MASK,
                    on_event,
                    info as *const core::ffi::c_void,
                );
                if !t.is_null() {
                    break t;
                }
                tap_log("创建失败（多半辅助功能未授权），2s 后重试");
                std::thread::sleep(std::time::Duration::from_millis(2000));
            };
            TAP_PORT.store(tap as usize, Ordering::Relaxed); // 超时自愈要用
            tap_log("事件监听已启动");
            let rl = CFRunLoopGetCurrent();
            let src = CFMachPortCreateRunLoopSource(std::ptr::null(), tap, 0);
            if src.is_null() {
                eprintln!("[tap] runloop source 创建失败");
                return;
            }
            CFRunLoopAddSource(rl, src, kCFRunLoopCommonModes);
            TAP_RUNLOOP.store(rl as usize, Ordering::Relaxed);
            CFRunLoopRun();
            // 走到这 = 看门狗停了我们（授权没了）：拆旧 tap，回外环重试
            TAP_RUNLOOP.store(0, Ordering::Relaxed);
            TAP_PORT.store(0, Ordering::Relaxed);
            CFRunLoopRemoveSource(rl, src, kCFRunLoopCommonModes);
            CFMachPortInvalidate(tap);
            CFRelease(src);
            CFRelease(tap);
            tap_log("授权丢失，tap 已拆除——等待重新授权重建");
        }
    }
}

// 启动 tap 线程（专职跑 CFRunLoop）+ 撤销看门狗。Box::into_raw 是有意的泄露：AppHandle 与进程同寿。
pub fn spawn(app: AppHandle) {
    let carrier = SendPtr(Box::into_raw(Box::new(app)));
    std::thread::spawn(move || run_tap(carrier));
    std::thread::spawn(|| loop {
        // 2s 与创建重试同拍；AXIsProcessTrusted 是 TCC 缓存读（微秒级）无副作用。
        // 主动 tap 的创建与存活都系于辅助功能，翻 false = tap 已死/将死——停 runloop
        // 交给外环重建，授权回来自动复活（撤销→重开辅助功能不再需要重启 app）
        std::thread::sleep(std::time::Duration::from_millis(2000));
        let rl = TAP_RUNLOOP.load(Ordering::Relaxed) as *const core::ffi::c_void;
        if !rl.is_null() && !crate::inject::accessibility_ok() {
            tap_log("看门狗：辅助功能已撤销，停 tap 待重建");
            unsafe { CFRunLoopStop(rl) };
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    // registered_combo 是吞键边界的唯一判据：命中 = 吞。用冷门键码播种全局集，
    // 避免与并行测试/真实配置互相干扰（测试结束恢复原集，防污染其他用例）
    #[test]
    fn 吞键判定_只命中已注册组合_其余放行() {
        let saved_d = DICTATION.lock().map(|s| s.clone()).ok();
        // 冷门键码 F13(105)——带修饰符位
        set_dictation(vec![(MOD_OPTION, 105)], false);
        assert!(registered_combo((MOD_OPTION, 105)), "听写键命中 → 吞");
        // 修饰符不同 / 键码不同 / 裸按（无修饰符）→ 一律放行
        assert!(!registered_combo((MOD_OPTION, 107)));
        assert!(!registered_combo((MOD_COMMAND, 105)));
        assert!(!registered_combo((0, 105)));
        assert!(!registered_combo((MOD_OPTION, 49)), "未注册的普通键放行");
        // 恢复全局集（尽力而为，防污染其他用例）
        set_dictation(saved_d.unwrap_or_default(), false);
    }
}
