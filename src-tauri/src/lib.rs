// 应用入口：注册插件、托盘、命令、全局状态——把各模块组装成完整应用
mod audio;
mod commands;
mod config;
mod engines;
mod hotkey;
mod inject;
mod models;
mod state;
mod stt;

use tauri::{
    menu::{Menu, MenuItem},
    tray::TrayIconBuilder,
    Manager,
};

fn open_settings(app: &tauri::AppHandle) {
    if let Some(win) = app.get_webview_window("settings") {
        let _ = win.show();
        let _ = win.unminimize();
        let _ = win.set_focus();
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(
            tauri_plugin_global_shortcut::Builder::new()
                .build(),
        )
        .plugin(tauri_plugin_dialog::init())
        .manage::<state::SharedState>(std::sync::Mutex::new(state::AppState::new()))
        // 设置窗口点 ✕ 只是隐藏（否则窗口被销毁，托盘再点就打不开了）
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                if window.label() == "settings" {
                    api.prevent_close();
                    let _ = window.hide();
                }
            }
        })
        .setup(|app| {
            // 托盘应用形态：不在 Dock 显示图标
            #[cfg(target_os = "macos")]
            app.set_activation_policy(tauri::ActivationPolicy::Accessory);

            // 启动即读一次配置——首次运行会自动落一份默认 config.json
            let _ = config::load(app.handle());

            // 收割上次运行（dev 硬重启/崩溃）留下的孤儿 llama-server
            stt::local::reap_orphan_engines(app.handle());

            // 引擎预热：启动即后台装载模型，第一次听写就是热的
            stt::local::warmup(app.handle());

            // 编译快捷键匹配集并发布给事件 tap（单键失败汇总日志，不让应用起不来）
            if let Err(e) = hotkey::register_all(app.handle()) {
                eprintln!("[hotkey] {e}");
            }

            // 按键事件统一入口（CGEventTap：组合触发/Fn/捕获/Esc 中止）
            hotkey::tap::spawn(app.handle().clone());
            // Menu 键(110) 状态轮询兜底（实测它不派发 keyDown 事件，tap 看不见）
            hotkey::fn_listener::spawn_monitor(app.handle().clone());

            // 菜单栏托盘
            let open = MenuItem::with_id(app, "open", "打开设置…", true, None::<&str>)?;
            let quit = MenuItem::with_id(app, "quit", "退出 Sayit", true, None::<&str>)?;
            let menu = Menu::with_items(app, &[&open, &quit])?;

            TrayIconBuilder::with_id("main-tray")
                .icon(app.default_window_icon().expect("没有默认图标").clone())
                .menu(&menu)
                .on_menu_event(|app, event| match event.id.as_ref() {
                    "open" => open_settings(app),
                    "quit" => app.exit(0),
                    _ => {}
                })
                .build(app)?;

            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::get_config,
            commands::save_config,
            commands::detect_engine,
            commands::toggle_recording,
            commands::hide_panel,
            commands::resize_panel,
            commands::mic_selftest,
            commands::open_mic_settings,
            commands::get_model_library,
            commands::download_model,
            commands::cancel_download,
            commands::delete_model,
            commands::add_user_model,
            commands::set_default_model,
            commands::start_hotkey_capture,
            commands::stop_hotkey_capture,
            commands::open_keyboard_settings,
            commands::copy_text,
            commands::open_settings,
            commands::open_accessibility_settings,
            commands::accessibility_status
        ])
        .build(tauri::generate_context!())
        .expect("error while running tauri application")
        .run(|app_handle, event| {
            // 退出应用时带走 llama-server 子进程，不留孤儿
            if let tauri::RunEvent::Exit = event {
                stt::local::kill_engine(&app_handle);
            }
        });
}
