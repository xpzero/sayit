// Menu 键(110) 状态轮询兜底
// 探针两轮实测：此键更新 HID 键码状态（CGEventSourceKeyState 可读）但不派发 keyDown
// 事件——CGEventTap 看不见它，只能轮询。全应用只有这一个键走这条路；
// 其余按键（含 Fn 的 flagsChanged）都由 tap.rs 的事件回调处理。
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use tauri::{AppHandle, Emitter};

use super::should_handle;
use super::tap;

unsafe extern "C" {
    fn CGEventSourceKeyState(state_id: i32, key: u16) -> bool;
}

const STATE: i32 = 1; // kCGEventSourceStateHIDSystemState（硬件层状态——查物理按键的标准源）
const CTX_MENU: u16 = 110;

static MENU_ON: AtomicBool = AtomicBool::new(false);

pub fn set_menu_enabled(v: bool) {
    MENU_ON.store(v, Ordering::Relaxed);
}

// 20ms 轮询 Menu 键边沿。tap 若也能收到（实测多数情况收不到），
// 触发侧靠 should_handle 的 300ms 防抖去重，捕获侧靠前端按名字去重。
pub fn spawn_monitor(app: AppHandle) {
    std::thread::spawn(move || {
        let mut last = false;
        loop {
            std::thread::sleep(Duration::from_millis(20));
            let now = unsafe { CGEventSourceKeyState(STATE, CTX_MENU) };
            let rising = now && !last;
            last = now;
            if !rising {
                continue;
            }
            if tap::capturing() {
                let _ = app.emit(
                    "hotkey-captured",
                    serde_json::json!({ "key": "ContextMenu" }),
                );
            } else if MENU_ON.load(Ordering::Relaxed) && should_handle() {
                // 注入链路（enigo→HIToolbox）是 main-thread-only，动作必须回主线程
                let app2 = app.clone();
                let _ = app.run_on_main_thread(move || {
                    crate::commands::toggle_recording_impl(&app2)
                });
            }
        }
    });
}
