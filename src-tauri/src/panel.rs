// 面板窗口管理 —— 由 commands.rs 拆出（结构铁律 8：单一职责；搬家零行为变化）
// show/hide/摆位：窗口几何恒定（520×170），一切状态切换都是胶囊在固定窗口内的 CSS 动效。
use tauri::{AppHandle, Emitter, Manager};

#[tauri::command]
pub fn hide_panel(app: AppHandle) {
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
    // 摆位只在启动时做一次（position_panel）；此处纯显示——面板出现的路径上
    // 没有任何 set_size/set_position，物理上不存在几何抖动源
    if let Some(win) = app.get_webview_window("panel") {
        let _ = win.show();
    }
}
