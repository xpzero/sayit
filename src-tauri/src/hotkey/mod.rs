// 快捷键模块：方言字符串 -> (修饰符位, 键码) 的编译 + 匹配集发布。
// 一切按键事件走 tap（CGEventTap，见 tap.rs）；
// "Fn" 是特殊令牌（flagsChanged 触发）、Menu 键(110) 由 fn_listener 状态轮询兜底。
pub mod fn_listener;
pub mod tap;

use std::str::FromStr;
use std::sync::atomic::{AtomicU64, Ordering};
use tauri::AppHandle;
use tauri_plugin_global_shortcut::Code; // 只用它的键名枚举与 from_str，不再注册 Carbon 热键

use crate::config::{self, Config};

// 防抖：按住连发只当一次（300ms 内的重复触发忽略）
static LAST_TRIGGER_MS: AtomicU64 = AtomicU64::new(0);

pub fn should_handle() -> bool {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;
    let last = LAST_TRIGGER_MS.load(Ordering::Relaxed);
    if now.saturating_sub(last) < 300 {
        return false;
    }
    // CAS 而非 store：tap 与 Menu 轮询双路径并发穿过检查窗口时，只有一个赢家（防 toggle 双发）
    LAST_TRIGGER_MS
        .compare_exchange(last, now, Ordering::Relaxed, Ordering::Relaxed)
        .is_ok()
}

// macOS 虚拟键码 <-> W3C Code 名（取自 global-hotkey 插件的 macOS 映射 + 探针实测的 110）
// 全应用唯一的键码真相表：触发匹配（方言->键码）与捕获命名（键码->方言）共用。
// KeyA(0) 缺席是刻意的：该键码有"无人触碰也读 true"的幽灵怪癖（探针两轮实测）。
const KEYCODES: &[(u16, &str)] = &[
    // 字母
    (1, "KeyS"), (2, "KeyD"), (3, "KeyF"), (4, "KeyH"), (5, "KeyG"),
    (6, "KeyZ"), (7, "KeyX"), (8, "KeyC"), (9, "KeyV"), (11, "KeyB"),
    (12, "KeyQ"), (13, "KeyW"), (14, "KeyE"), (15, "KeyR"), (16, "KeyY"), (17, "KeyT"),
    (31, "KeyO"), (32, "KeyU"), (34, "KeyI"), (35, "KeyP"),
    (37, "KeyL"), (38, "KeyJ"), (40, "KeyK"), (45, "KeyN"), (46, "KeyM"),
    // 数字
    (18, "Digit1"), (19, "Digit2"), (20, "Digit3"), (21, "Digit4"),
    (23, "Digit5"), (22, "Digit6"), (26, "Digit7"), (28, "Digit8"), (25, "Digit9"), (29, "Digit0"),
    // 符号
    (24, "Equal"), (27, "Minus"), (30, "BracketRight"), (33, "BracketLeft"),
    (39, "Quote"), (41, "Semicolon"), (42, "Backslash"),
    (43, "Comma"), (44, "Slash"), (47, "Period"), (50, "Backquote"),
    // 控制键
    (36, "Enter"), (48, "Tab"), (49, "Space"), (51, "Backspace"), (53, "Escape"),
    // F 键
    (122, "F1"), (120, "F2"), (99, "F3"), (118, "F4"), (96, "F5"), (97, "F6"),
    (98, "F7"), (100, "F8"), (101, "F9"), (109, "F10"), (103, "F11"), (111, "F12"),
    (105, "F13"), (107, "F14"), (113, "F15"), (106, "F16"), (64, "F17"), (79, "F18"), (80, "F19"), (90, "F20"),
    // 导航（PC 键盘区）
    (114, "Insert"), (115, "Home"), (116, "PageUp"), (117, "Delete"), (119, "End"), (121, "PageDown"),
    // 方向键
    (123, "ArrowLeft"), (124, "ArrowRight"), (125, "ArrowDown"), (126, "ArrowUp"),
    // 小键盘
    (65, "NumpadDecimal"), (67, "NumpadMultiply"), (69, "NumpadAdd"), (71, "NumpadClear"),
    (75, "NumpadDivide"), (76, "NumpadEnter"), (78, "NumpadSubtract"), (81, "NumpadEqual"),
    (82, "Numpad0"), (83, "Numpad1"), (84, "Numpad2"), (85, "Numpad3"), (86, "Numpad4"),
    (87, "Numpad5"), (88, "Numpad6"), (89, "Numpad7"), (91, "Numpad8"), (92, "Numpad9"),
    // Menu 键：探针实测键码 110（状态可见、无 keyDown 事件，由 fn_listener 轮询兜底）
    (110, "ContextMenu"),
];

// 键码 -> Code 名（捕获时给按键起名）
pub fn key_name(keycode: u16) -> Option<&'static str> {
    KEYCODES.iter().find(|(c, _)| *c == keycode).map(|(_, n)| *n)
}

// Code 名 -> 键码（借助 Display 打出名字查表，省掉一份镜像表）
fn keycode_of(code: &Code) -> Option<u16> {
    let name = code.to_string();
    KEYCODES.iter().find(|(_, n)| *n == name).map(|(c, _)| *c)
}

// 方言组合 -> (修饰符位, 键码)。修饰符别名自己认（cmd/⌘ 这类系统不管）；
// 主键交给标准库 Code::from_str——它认识整个 W3C 键表，键码表里没有的键会报"不支持"。
pub fn compile(s: &str) -> Result<(u64, u16), String> {
    let mut mods: u64 = 0;
    let mut code: Option<Code> = None;

    for part in s.split('+').map(str::trim).filter(|p| !p.is_empty()) {
        let pl = part.to_lowercase(); // 修饰符大小写不敏感
        let modifier: Option<u64> = match pl.as_str() {
            "ctrl" | "control" => Some(tap::MOD_CTRL),
            "shift" => Some(tap::MOD_SHIFT),
            "alt" | "opt" | "option" => Some(tap::MOD_OPTION),
            "meta" | "cmd" | "command" | "super" | "win" => Some(tap::MOD_COMMAND),
            _ => None,
        };
        if let Some(m) = modifier {
            mods |= m;
            continue;
        }
        if code.is_some() {
            return Err(format!("『{s}』里有多个主键"));
        }
        code = Some(parse_code(part)?); // 主键保留原文：PageUp 这类驼峰名要靠大小写匹配
    }

    let code = code.ok_or_else(|| format!("『{s}』缺少主键"))?;
    let keycode = keycode_of(&code).ok_or_else(|| {
        format!("『{s}』的主键 {code} 在 macOS 键码表里不存在，无法注册")
    })?;
    if code == Code::Space && mods == 0 {
        return Err("裸空格不能做全局快捷键（否则打字会一直触发）".into());
    }
    Ok((mods, keycode))
}

fn parse_code(p: &str) -> Result<Code, String> {
    let pl = p.to_lowercase();
    // 系统层进不了组合的键，提前给出人话
    if pl == "fn" {
        return Err("Fn 走专用监听器，不能进组合键（单独绑定它即可）".into());
    }

    // 单字符字母/数字：方言用短名（"a"/"A" -> KeyA、"1" -> Digit1）
    let mut chars = pl.chars();
    if let (Some(c), None) = (chars.next(), chars.next()) {
        if c.is_ascii_alphabetic() {
            return Code::from_str(&format!("Key{}", c.to_ascii_uppercase()))
                .map_err(|_| format!("不认识的键：{p}"));
        }
        if c.is_ascii_digit() {
            return Code::from_str(&format!("Digit{c}")).map_err(|_| format!("不认识的键：{p}"));
        }
    }

    // 历史方言别名 -> 标准 Code 字符串（旧配置迁移用；"delete" 现指 PC 正向删除，退格是 backspace）
    let canonical: Option<&str> = match pl.as_str() {
        "esc" => Some("Escape"),
        "return" => Some("Enter"),
        "menu" => Some("ContextMenu"),
        "grave" | "`" => Some("Backquote"),
        "left" => Some("ArrowLeft"),
        "right" => Some("ArrowRight"),
        "up" => Some("ArrowUp"),
        "down" => Some("ArrowDown"),
        "," => Some("Comma"),
        "." => Some("Period"),
        "/" => Some("Slash"),
        ";" => Some("Semicolon"),
        "'" => Some("Quote"),
        "-" => Some("Minus"),
        "=" => Some("Equal"),
        "[" => Some("BracketLeft"),
        "]" => Some("BracketRight"),
        "\\" => Some("Backslash"),
        _ => None,
    };

    // 其余全部交给标准库键表：原文优先（PageUp/Numpad7 驼峰直接命中），
    // 再试首字母大写兜底手写大小写（"f13" -> "F13"）
    let name = canonical.unwrap_or(p);
    let titled = {
        let mut cs = name.chars();
        match cs.next() {
            Some(f) => format!("{}{}", f.to_uppercase(), cs.as_str()),
            None => name.to_string(),
        }
    };
    Code::from_str(name)
        .or_else(|_| Code::from_str(&titled))
        .map_err(|_| format!("不认识的键：{p}"))
}

// 按配置编译匹配集并热发布给 tap。单键失败不拖累其他键（错误汇总上报）。
pub fn register_all(app: &AppHandle) -> Result<(), String> {
    let cfg: Config = config::load(app);

    let mut combos: Vec<(u64, u16)> = Vec::new();
    let mut fn_on = false;
    let mut errors: Vec<String> = Vec::new();
    for key in &cfg.hotkeys.dictation {
        if key.eq_ignore_ascii_case("fn") {
            fn_on = true;
            continue;
        }
        match compile(key) {
            Ok(c) => {
                if !combos.contains(&c) {
                    combos.push(c);
                }
            }
            Err(e) => errors.push(e),
        }
    }

    // Menu 键轮询兜底开关（tap 若能收到 110 的 keyDown 也会触发，300ms 防抖去重）
    fn_listener::set_menu_enabled(combos.iter().any(|(_, kc)| *kc == tap::K_CTX_MENU));
    tap::set_dictation(combos, fn_on);

    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors.join("；"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compile_covers_dialect_and_new_keys() {
        // 历史方言（大小写与符号别名）
        assert_eq!(compile("Option+Space").unwrap(), (tap::MOD_OPTION, 49));
        assert_eq!(compile("ctrl+shift+s").unwrap(), (tap::MOD_CTRL | tap::MOD_SHIFT, 1)); // KeyS=1
        assert_eq!(compile("cmd+opt+1").unwrap(), (tap::MOD_COMMAND | tap::MOD_OPTION, 18));
        assert!(compile("esc").is_ok());
        assert!(compile("return").is_ok());
        assert!(compile("left").is_ok());
        assert!(compile(",").is_ok());
        assert!(compile("`").is_ok());

        // 新增：外置键盘键 + W3C 全键表
        for s in [
            "F13", "F19", "F20", "Home", "End", "PageUp", "PageDown", "Insert", "Delete",
            "Backspace", "Backquote", "Numpad7", "NumpadEnter", "NumpadDecimal", "ArrowLeft",
            "Option+ArrowUp", "f13",
        ] {
            assert!(compile(s).is_ok(), "应可编译：{s}");
        }
        // Menu 键（探针实测键码 110）
        assert_eq!(compile("ContextMenu").unwrap(), (0, 110));
        assert_eq!(compile("Menu").unwrap(), (0, 110));
    }

    #[test]
    fn compile_rejects_bad_input() {
        assert!(compile("Space").is_err(), "裸空格");
        assert!(compile("Fn+A").is_err(), "Fn 不进组合");
        assert!(compile("Fn").is_err(), "Fn 不可编译");
        assert!(compile("a").is_err(), "A 键码 0 是幽灵键，不可绑");
        assert!(compile("NoSuchKey").is_err());
        assert!(compile("A+B").is_err(), "多个主键");
        assert!(compile("Ctrl").is_err(), "缺主键");
    }

    #[test]
    fn delete_means_forward_delete() {
        // PC 的 Del 是正向删除(117)；退格是 Backspace(51)
        assert_eq!(compile("Delete").unwrap().1, 117);
        assert_eq!(compile("Backspace").unwrap().1, 51);
    }

    #[test]
    fn keycode_table_is_sound() {
        // 键码不重复 + 每个名字都能被标准库解析（写错立刻红）
        let mut seen = std::collections::HashSet::new();
        for (kc, name) in KEYCODES {
            assert!(seen.insert(*kc), "键码 {kc} 重复");
            assert!(Code::from_str(name).is_ok(), "『{name}』不是合法 Code 名");
        }
        // 幽灵键码 0 必须缺席
        assert!(!seen.contains(&0), "键码 0（幽灵）不得入表");
    }
}

// ---------- 快捷键捕获命令（设置页录制框的开关） ----------
#[tauri::command]
pub fn start_hotkey_capture() {
    crate::hotkey::tap::start_capture();
}

#[tauri::command]
pub fn stop_hotkey_capture() {
    crate::hotkey::tap::stop_capture();
}

