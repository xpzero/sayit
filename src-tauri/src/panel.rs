// 面板窗口管理 —— 由 commands.rs 拆出（结构铁律 8：单一职责；搬家零行为变化）
// show/hide/摆位：窗口几何恒定（520×170），一切状态切换都是胶囊在固定窗口内的 CSS 动效。
use tauri::{AppHandle, Emitter, Manager};

// 面板可见性镜像（tap 线程无锁读）：全部 show/hide 都流经本文件，镜像即真相。
// tap 回调禁锁（等锁超时会被系统禁 tap，AGENTS 在案），所以可见性用原子量；
// 几何在启动摆位后写入 OnceLock（读取等待无关奔命，lock-free）
static PANEL_VISIBLE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
static PANEL_FRAME: std::sync::OnceLock<[f64; 4]> = std::sync::OnceLock::new(); // x, y, w, h（CG 系：原点左上，逻辑点）

/// 点到面板外 → 收板（tap 的鼠标分支调用，坐标为 CG 系）。只在空闲期收：听写中面板是
/// 工作 UI，不能被外部点击误伤。收板动作派回主线程（NSWindow 操作不许在 tap 线程做）
pub fn try_click_outside_hide(app: &AppHandle, x: f64, y: f64) {
    use std::sync::atomic::Ordering;
    if !PANEL_VISIBLE.load(Ordering::Relaxed) || crate::commands::ACTIVE.load(Ordering::Relaxed) {
        return;
    }
    let inside = PANEL_FRAME
        .get()
        .map(|f| x >= f[0] && x <= f[0] + f[2] && y >= f[1] && y <= f[1] + f[3])
        .unwrap_or(true); // 几何未知时保守不收（宁可多留 5s，不误收）
    if !inside {
        println!("[tap] 面板外点击：收起提示面板");
        let app2 = app.clone();
        let _ = app.run_on_main_thread(move || {
            hide_panel(app2);
        });
    }
}

#[tauri::command]
pub fn hide_panel(app: AppHandle) {
    PANEL_VISIBLE.store(false, std::sync::atomic::Ordering::Relaxed);
    if let Some(win) = app.get_webview_window("panel") {
        let _ = win.hide();
    }
}

// 打开设置页（可选直达某个 tab：rec/model/key/cloud/perm/about）
#[tauri::command]
pub fn open_settings(app: AppHandle, tab: Option<String>) {
    if let Some(win) = app.get_webview_window("settings") {
        let _ = win.show();
        let _ = win.set_focus();
        if let Some(t) = tab {
            let _ = app.emit("goto-tab", serde_json::json!({ "tab": t }));
        }
    }
}


// 面板窗口固定尺寸（520×170，见 tauri.conf.json）：一切状态切换都是胶囊在固定窗口内
// 的 CSS 动效。动态贴合内容的三种实现（瞬移/步进动画/透明帧掩护瞬移）均实测有肉眼
// 抖动——窗口服务 set_size/set_position 异步非原子 + WKWebView 陈旧帧拉伸/视口异步，
// 竞态砍不尽；固定几何是唯一稳定解（详见 AGENTS.md「面板窗口」条）。
const PANEL_BOTTOM_GAP: f64 = 96.0; // 距屏幕底部留白（避开 Dock 区域）

// 主屏底部居中摆位（先摆位再显示，避免闪跳）
fn center_panel_bottom(win: &tauri::WebviewWindow, w: f64, h: f64) {
    if let Ok(Some(mon)) = win.primary_monitor() {
        let sf = mon.scale_factor();
        let mw = mon.size().width as f64 / sf;
        let mh = mon.size().height as f64 / sf;
        let x = (mw - w) / 2.0;
        let y = (mh - h - PANEL_BOTTOM_GAP).max(0.0);
        let _ = win.set_position(tauri::LogicalPosition::new(x, y));
        // 几何镜像（tap 点外收板用）用**计算值**，绝不回读——set_position 在 macOS
        // 异步生效，紧跟着的 outer_position 拿到的是搬家前旧坐标；镜像错位后面板内
        // 每次点击都被判成"点外"而在 mousedown 时收板，按钮 click 永远走不完
        // （复制按钮"点了没反应但浮板消失"的真因，2026-11-11 实测）。坐标系与
        // CGEventGetLocation 同系：主屏左上原点、逻辑点
        let _ = PANEL_FRAME.set([x, y, w, h]);
    }
}

// 面板 NSPanel 化（2026-11-11，tauri-nspanel v2.1）：tao 只会建 NSWindow，点击面板按钮
// 会激活整个应用（顶起设置窗、抢键盘焦点、闪菜单栏——WindowServer 实证；styleMask 补位
// 在普通 NSWindow 上无效，已实测否决）。插件把窗口类 swizzle 成自定义 NSPanel 子类，
// config 直接把 can_become_key_window 钉死 false（保留 focusable:false 语义：键盘焦点
// 永远留在用户的应用），再补 Nonactivating 掩码（点击不激活应用）。
// 观测点 [panel-style]。插件无 Drop：句柄丢弃后类替换仍持效（进程同寿）。
tauri_nspanel::tauri_panel! {
    panel!(DictationPanel {
        config: {
            can_become_key_window: false,
            can_become_main_window: false,
            hides_on_deactivate: false
        }
    })
}

pub fn convert_panel_to_nspanel(app: &AppHandle) {
    use tauri_nspanel::Panel as _; // trait 方法（add_style_mask / can_become_key_window）
    use tauri_nspanel::objc2_app_kit::NSWindowStyleMask;
    let Some(win) = app.get_webview_window("panel") else {
        eprintln!("[panel-style] 拿不到面板窗口，NSPanel 化跳过");
        return;
    };
    match DictationPanel::from_window(&win) {
        Ok(panel) => {
            let key_ok = panel.can_become_key_window();
            match panel.add_style_mask(NSWindowStyleMask::NonactivatingPanel) {
                Ok(()) => println!(
                    "[panel-style] NSPanel 化完成：nonactivating 已加，canBecomeKey={key_ok}（应为 false）"
                ),
                Err(e) => eprintln!("[panel-style] nonactivating 掩码失败（{e:?}）：点击仍可能激活应用"),
            }
        }
        Err(e) => eprintln!("[panel-style] from_window 失败（{e:?}），面板保持普通窗口"),
    }
}

/// 启动时摆一次面板位（窗口几何恒定，之后 show/hide 不再碰位置——show 路径零窗口操作）
pub fn position_panel(app: &AppHandle) {
    let Some(win) = app.get_webview_window("panel") else {
        return;
    };
    if let Ok(size) = win.outer_size() {
        let sf = win
            .primary_monitor()
            .ok()
            .flatten()
            .map(|m| m.scale_factor())
            .unwrap_or(2.0);
        center_panel_bottom(&win, size.width as f64 / sf, size.height as f64 / sf);
    }
}

pub fn show_panel(app: &AppHandle) {
    PANEL_VISIBLE.store(true, std::sync::atomic::Ordering::Relaxed);
    // 摆位只在启动时做一次（position_panel）；此处纯显示——面板出现的路径上
    // 没有任何 set_size/set_position，物理上不存在几何抖动源
    if let Some(win) = app.get_webview_window("panel") {
        let _ = win.show();
    }
}
