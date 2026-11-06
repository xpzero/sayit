// 快捷键模块：把配置里的方言字符串（"Option+Space"）翻译成系统认识的
// 组合键对象并注册全局监听。Fn 键走专用的轮询监听（fn_listener）。
pub mod fn_listener;

use std::sync::atomic::{AtomicU64, Ordering};
use tauri::AppHandle;
use tauri_plugin_global_shortcut::{
    Code, GlobalShortcutExt, Modifiers, Shortcut, ShortcutState,
};

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
    LAST_TRIGGER_MS.store(now, Ordering::Relaxed);
    true
}

// 按配置重新注册全部快捷键（保存设置后调用；启动时也调用）
pub fn register_all(app: &AppHandle) -> Result<(), String> {
    let cfg: Config = config::load(app);
    let gs = app.global_shortcut();

    // 先把旧的全部注销（返回值忽略：可能本来就没注册过）
    let _ = gs.unregister_all();

    let mut registered = Vec::new();
    for key in &cfg.hotkeys.dictation {
        // "Fn" 是特殊令牌：插件注册不了它，由轮询监听器接管
        if key.eq_ignore_ascii_case("fn") {
            continue;
        }
        let shortcut = parse(key)?;
        gs.on_shortcut(shortcut, |app, _sc, event| {
            if event.state == ShortcutState::Pressed && should_handle() {
                crate::commands::toggle_recording_impl(app);
            }
        })
        .map_err(|e| format!("注册 {key} 失败：{e}（可能被其他应用占用）"))?;
        registered.push(key.clone());
    }
    // 配置里含 Fn 令牌时，打开轮询监听器里的 Fn 开关
    fn_listener::set_fn_enabled(
        cfg.hotkeys.dictation.iter().any(|k| k.eq_ignore_ascii_case("fn")),
    );
    Ok(())
}

// 方言字符串 -> Shortcut 对象
// 自己解析而不是用库的 from_str：别名可控、报错可读（"option" 也认、"⌘" 的事以后再说）
pub fn parse(s: &str) -> Result<Shortcut, String> {
    let mut mods = Modifiers::empty();
    let mut code: Option<Code> = None;

    for part in s.split('+').map(str::trim).filter(|p| !p.is_empty()) {
        let p = part.to_lowercase();
        let modifier = match p.as_str() {
            "ctrl" | "control" => Some(Modifiers::CONTROL),
            "shift" => Some(Modifiers::SHIFT),
            "alt" | "opt" | "option" => Some(Modifiers::ALT),
            "meta" | "cmd" | "command" | "super" | "win" => Some(Modifiers::META),
            _ => None,
        };
        if let Some(m) = modifier {
            mods |= m;
            continue;
        }
        if code.is_some() {
            return Err(format!("『{s}』里有多个主键"));
        }
        code = Some(parse_code(&p)?);
    }

    let code = code.ok_or_else(|| format!("『{s}』缺少主键"))?;
    if code == Code::Space && mods.is_empty() {
        return Err("裸空格不能做全局快捷键（否则打字会一直触发）".into());
    }
    Ok(Shortcut::new(Some(mods), code))
}

fn parse_code(p: &str) -> Result<Code, String> {
    Ok(match p {
        "space" => Code::Space,
        "tab" => Code::Tab,
        "enter" | "return" => Code::Enter,
        "escape" | "esc" => Code::Escape,
        "delete" | "backspace" => Code::Backspace,
        "up" => Code::ArrowUp,
        "down" => Code::ArrowDown,
        "left" => Code::ArrowLeft,
        "right" => Code::ArrowRight,
        "comma" | "," => Code::Comma,
        "period" | "." => Code::Period,
        "slash" | "/" => Code::Slash,
        "semicolon" | ";" => Code::Semicolon,
        "quote" | "'" => Code::Quote,
        "backquote" | "`" => Code::Backquote,
        "minus" | "-" => Code::Minus,
        "equal" | "=" => Code::Equal,
        "bracketleft" | "[" => Code::BracketLeft,
        "bracketright" | "]" => Code::BracketRight,
        "backslash" | "\\" => Code::Backslash,
        _ => {
            // 单个字母 / 数字
            let mut chars = p.chars();
            if let (Some(c), None) = (chars.next(), chars.next()) {
                if c.is_ascii_alphabetic() {
                    return Ok(match c.to_ascii_uppercase() {
                        'A' => Code::KeyA, 'B' => Code::KeyB, 'C' => Code::KeyC,
                        'D' => Code::KeyD, 'E' => Code::KeyE, 'F' => Code::KeyF,
                        'G' => Code::KeyG, 'H' => Code::KeyH, 'I' => Code::KeyI,
                        'J' => Code::KeyJ, 'K' => Code::KeyK, 'L' => Code::KeyL,
                        'M' => Code::KeyM, 'N' => Code::KeyN, 'O' => Code::KeyO,
                        'P' => Code::KeyP, 'Q' => Code::KeyQ, 'R' => Code::KeyR,
                        'S' => Code::KeyS, 'T' => Code::KeyT, 'U' => Code::KeyU,
                        'V' => Code::KeyV, 'W' => Code::KeyW, 'X' => Code::KeyX,
                        'Y' => Code::KeyY, 'Z' => Code::KeyZ,
                        _ => unreachable!(),
                    });
                }
                if c.is_ascii_digit() {
                    return Ok(match c {
                        '0' => Code::Digit0, '1' => Code::Digit1, '2' => Code::Digit2,
                        '3' => Code::Digit3, '4' => Code::Digit4, '5' => Code::Digit5,
                        '6' => Code::Digit6, '7' => Code::Digit7, '8' => Code::Digit8,
                        _ => Code::Digit9,
                    });
                }
            }
            // F1..F19
            if let Some(num) = p.strip_prefix('f') {
                if let Ok(n) = num.parse::<u8>() {
                    if (1..=19).contains(&n) {
                        return Ok(f_key(n));
                    }
                }
            }
            return Err(format!("不认识的键：{p}"));
        }
    })
}

fn f_key(n: u8) -> Code {
    match n {
        1 => Code::F1, 2 => Code::F2, 3 => Code::F3, 4 => Code::F4, 5 => Code::F5,
        6 => Code::F6, 7 => Code::F7, 8 => Code::F8, 9 => Code::F9, 10 => Code::F10,
        11 => Code::F11, 12 => Code::F12, 13 => Code::F13, 14 => Code::F14,
        15 => Code::F15, 16 => Code::F16, 17 => Code::F17, 18 => Code::F18,
        _ => Code::F19,
    }
}
