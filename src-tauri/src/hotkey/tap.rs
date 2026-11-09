// CGEventTap：全局按键事件的统一入口（listen-only 会话 tap：不改写、不吞键）
// 取代 Carbon 全局快捷键注册与 20ms 轮询，一套逻辑处理：
//   - 组合触发：keyDown 的 (flags, keycode) 与匹配集比对
//   - Fn 触发/录入：flagsChanged（keycode 63 + SecondaryFn 位）
//   - Esc 中止录音、捕获模式录制（keyDown 预览 / keyUp 提交，自动重复字段精确去重）
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
}

const SESSION_TAP: i32 = 1; // kCGSessionEventTap
const HEAD_INSERT: i32 = 0; // kCGHeadInsertEventTap
const LISTEN_ONLY: isize = 1; // kCGEventTapOptionListenOnly（CFIndex/isize）
// CGEvent 的事件类型编号（CGEventTypes.h）：keyDown=10、keyUp=11、flagsChanged=12。
// 曾误写成 1/2/12——1/2 其实是左键按下/抬起（鼠标事件！）：tap 只看得见鼠标点击和 Fn，
// 字母/数字/空格组合键与 Esc 中止全部失灵；鼠标点击还顶着"键码 0"的马甲混进
// keyDown 通道（即 handle_key_down 里那只"幽灵键码 0"的真身）
const MASK: u64 = (1 << 10) | (1 << 11) | (1 << 12); // keyDown | keyUp | flagsChanged
const TAP_DISABLED_BY_TIMEOUT: u32 = 0xFFFF_FFE1; // kCGEventTapDisabledByTimeout
const TAP_DISABLED_BY_USER: u32 = 0xFFFF_FFE0; // kCGEventTapDisabledByUserInput

const T_KEYDOWN: u32 = 10;
const T_KEYUP: u32 = 11;
const T_FLAGS_CHANGED: u32 = 12;

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

// 回调不可 panic（跨 FFI = UB），所有分支只做匹配与 emit
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
        T_FLAGS_CHANGED => handle_flags_changed(app, keycode, flags),
        T_KEYDOWN => {
            if unsafe { CGEventGetIntegerValueField(event, F_AUTOREPEAT) } != 0 {
                return event; // 自动重复：精确丢弃
            }
            handle_key_down(app, keycode as u16, flags);
        }
        T_KEYUP => handle_key_up(app, keycode as u16),
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

fn handle_key_down(app: &AppHandle, kc: u16, flags: u64) {
    if kc == 0 {
        return; // 键码 0 防御性丢弃（历史上是鼠标点击混进来的"幽灵键"；见 MASK 注释）
    }
    if is_modifier_keycode(kc) {
        // Fn+修饰键手势不算"Fn 单独按"（否则按 Fn+Shift、先松 Shift 再松 Fn 会误录成裸 Fn）
        if capturing() {
            MAIN_SEEN.store(true, Ordering::Relaxed);
        }
        return;
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
        return;
    }

    // Esc 中止录音（进行中才动）。读无锁相位镜像而非 SharedState——
    // 回调等互斥锁超过 ~1s 会被系统禁用 tap，全部热键失灵
    if kc == K_ESCAPE {
        if crate::commands::ACTIVE.load(Ordering::Relaxed) {
            dispatch_abort(app);
        }
        return;
    }

    // 组合匹配触发
    let combo = (flags & MOD_ALL, kc);
    let hit = DICTATION
        .lock()
        .map(|set| set.iter().any(|c| *c == combo))
        .unwrap_or(false);
    if hit && should_handle() {
        dispatch_toggle(app);
    }
}

// keyUp 提交（按下抬起即录入；键码与最近一次 keyDown 一致才算）
fn handle_key_up(app: &AppHandle, kc: u16) {
    if !capturing() || kc == 0 || PENDING.load(Ordering::Relaxed) != kc {
        return;
    }
    PENDING.store(0, Ordering::Relaxed);
    if let Ok(mut g) = PENDING_COMBO.lock() {
        if let Some(name) = g.take() {
            let _ = app.emit("hotkey-captured", serde_json::json!({ "key": name }));
        }
    }
}

// 裸指针不是 Send，包一层带声明的载体（AppHandle 自身线程安全，指针只作携带）。
// 注意线程体必须是独立函数：Rust 2021 闭包按字段精确捕获，
// 若在闭包里解构 SendPtr 会直接捕获裸指针字段、绕过这里的 unsafe impl Send。
struct SendPtr(*const AppHandle);
unsafe impl Send for SendPtr {}

fn run_tap(carrier: SendPtr) {
    unsafe {
        let SendPtr(info) = carrier;
        let tap = CGEventTapCreate(
            SESSION_TAP,
            HEAD_INSERT,
            LISTEN_ONLY,
            MASK,
            on_event,
            info as *const core::ffi::c_void,
        );
        if tap.is_null() {
            eprintln!("[tap] 创建失败：无辅助功能权限？（触发与捕获将不可用）");
            return;
        }
        TAP_PORT.store(tap as usize, Ordering::Relaxed); // 超时自愈要用
        println!("[tap] 事件监听已启动");
        let rl = CFRunLoopGetCurrent();
        let src = CFMachPortCreateRunLoopSource(std::ptr::null(), tap, 0);
        if src.is_null() {
            eprintln!("[tap] runloop source 创建失败");
            return;
        }
        CFRunLoopAddSource(rl, src, kCFRunLoopCommonModes);
        CFRunLoopRun();
    }
}

// 启动 tap 线程（专职跑 CFRunLoop）。Box::into_raw 是有意的泄露：AppHandle 与进程同寿。
pub fn spawn(app: AppHandle) {
    let carrier = SendPtr(Box::into_raw(Box::new(app)));
    std::thread::spawn(move || run_tap(carrier));
}
