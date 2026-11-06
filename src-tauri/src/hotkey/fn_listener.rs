// Fn 监听 + 统一捕获模式（步骤 6）
// 实现取巧：不用 CGEventTap 回调，而是 20ms 轮询系统输入状态
// （CGEventSourceFlagsState / CGEventSourceKeyState 两个 CoreGraphics 函数，零依赖）
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter};

use super::should_handle;

// macOS CoreGraphics 的两个 C 函数——直接声明外部链接（Rust 与 C 的握手）
unsafe extern "C" {
    fn CGEventSourceFlagsState(state_id: i32) -> u64;
    fn CGEventSourceKeyState(state_id: i32, key: u16) -> bool;
}

const STATE: i32 = 1; // kCGEventSourceStateHIDSystemState（硬件层状态——查物理按键的标准源）
const FN_FLAG: u64 = 1 << 23; // kCGEventFlagMaskSecondaryFn（Fn/Globe 键的标志位）

// 主键表：macOS 虚拟键码 -> 方言名（与 hotkey::parse 认的名字一致）
// 注意：键码 0（字母A）被除名——实测此键码在无人触碰时恒读 true（系统怪癖），
// 且它的存在会让"无主键"判断永远为假，毒化 Fn 录入。代价：A 不能当主键（无人这么用）。
const KEYS: &[(u16, &str)] = &[
    // 字母（ANSI 布局的经典键码表，缺 A 见上注）
    (1, "S"), (2, "D"), (3, "F"), (4, "H"), (5, "G"),
    (6, "Z"), (7, "X"), (8, "C"), (9, "V"), (11, "B"),
    (12, "Q"), (13, "W"), (14, "E"), (15, "R"), (16, "Y"), (17, "T"),
    (31, "O"), (32, "U"), (34, "I"), (35, "P"),
    (37, "L"), (38, "J"), (40, "K"), (45, "N"), (46, "M"),
    // 数字
    (18, "1"), (19, "2"), (20, "3"), (21, "4"), (23, "5"), (22, "6"),
    (26, "7"), (28, "8"), (25, "9"), (29, "0"),
    // 常用符号
    (24, "Equal"), (27, "Minus"), (30, "BracketRight"), (33, "BracketLeft"),
    (39, "Quote"), (41, "Semicolon"), (42, "Backslash"),
    (43, "Comma"), (44, "Slash"), (47, "Period"),
    // 控制键
    (36, "Enter"), (48, "Tab"), (49, "Space"),
    // F 键
    (122, "F1"), (120, "F2"), (99, "F3"), (118, "F4"), (96, "F5"), (97, "F6"),
    (98, "F7"), (100, "F8"), (101, "F9"), (109, "F10"), (103, "F11"), (111, "F12"),
    // 方向键
    (123, "Left"), (124, "Right"), (125, "Down"), (126, "Up"),
];
const ESC: u16 = 53;

static FN_ENABLED: AtomicBool = AtomicBool::new(false);
static CAPTURING: AtomicBool = AtomicBool::new(false);

pub fn set_fn_enabled(v: bool) {
    FN_ENABLED.store(v, Ordering::Relaxed);
}

pub fn start_capture() {
    CAPTURING.store(true, Ordering::Relaxed);
}

pub fn stop_capture() {
    CAPTURING.store(false, Ordering::Relaxed);
}

fn flags() -> u64 {
    unsafe { CGEventSourceFlagsState(STATE) }
}

fn key_down(code: u16) -> bool {
    unsafe { CGEventSourceKeyState(STATE, code) }
}

// 从修饰符标志位里拼出方言前缀（Fn 不进组合——系统会吞 Fn+键，组合里放不下它）
fn mod_names(f: u64) -> Vec<&'static str> {
    let mut m = Vec::new();
    if f & (1 << 18) != 0 {
        m.push("Ctrl");
    }
    if f & (1 << 17) != 0 {
        m.push("Shift");
    }
    if f & (1 << 19) != 0 {
        m.push("Option");
    }
    if f & (1 << 20) != 0 {
        m.push("Command");
    }
    m
}

// 唯一的输入监视线程：平时盯 Fn 边沿；录制框激活时进入捕获模式记录一切按键
pub fn spawn_monitor(app: AppHandle) {
    std::thread::spawn(move || {
        let mut last_fn = false;
        let mut prev_pressed: Vec<u16> = Vec::new(); // 上一拍按住的键集合
        let mut fn_alone_since: Option<Instant> = None;
        let mut was_capturing = false;

        loop {
            std::thread::sleep(Duration::from_millis(20));
            let f = flags();
            let fn_now = f & FN_FLAG != 0;
            let capturing = CAPTURING.load(Ordering::Relaxed);

            // Fn 边沿在循环顶部先消费掉（无论走哪个分支）——
            // 否则"录完 Fn 松手前的残留按住"会在捕获结束后被误判成新触发
            let fn_rising = fn_now && !last_fn;
            last_fn = fn_now;
            if !capturing && fn_rising && FN_ENABLED.load(Ordering::Relaxed) && should_handle() {
                crate::commands::toggle_recording_impl(&app);
            }

            // 捕获模式开关的时刻：清旧账（已经按着的键不算新按键）
            if capturing != was_capturing {
                was_capturing = capturing;
                fn_alone_since = None;
                prev_pressed = KEYS
                    .iter()
                    .filter(|(c, _)| key_down(*c))
                    .map(|(c, _)| *c)
                    .collect();
            }

            // ---------- 捕获模式（录制框） ----------
            if capturing {
                // Esc 取消录制
                if key_down(ESC) {
                    CAPTURING.store(false, Ordering::Relaxed);
                    let _ = app.emit("hotkey-captured", serde_json::json!({ "cancelled": true }));
                    prev_pressed.clear();
                    continue;
                }

                // 当前按住的键集合
                let cur: Vec<(u16, &str)> =
                    KEYS.iter().filter(|(c, _)| key_down(*c)).copied().collect();

                // 上升沿 = 本次按住、上一拍没有的键（跨键表顺序免疫）
                if let Some((_code, name)) = cur.iter().find(|(c, _)| !prev_pressed.contains(c)) {
                    let mut parts = mod_names(f);
                    parts.push(name);
                    let combo = parts.join("+");
                    CAPTURING.store(false, Ordering::Relaxed);
                    let _ = app.emit("hotkey-captured", serde_json::json!({ "key": combo }));
                    prev_pressed.clear();
                    continue;
                }
                prev_pressed = cur.iter().map(|(c, _)| *c).collect();

                // 没有主键时：只按着 Fn 且持续 300ms -> 录入特殊令牌 "Fn"
                if cur.is_empty() && fn_now && mod_names(f).is_empty() {
                    match fn_alone_since {
                        Some(t) if t.elapsed() >= Duration::from_millis(300) => {
                            CAPTURING.store(false, Ordering::Relaxed);
                            let _ = app.emit(
                                "hotkey-captured",
                                serde_json::json!({ "key": "Fn" }),
                            );
                            fn_alone_since = None;
                        }
                        None => fn_alone_since = Some(Instant::now()),
                        _ => {}
                    }
                } else {
                    fn_alone_since = None;
                }
                continue;
            }
        }
    });
}
