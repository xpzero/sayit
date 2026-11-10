// 应用入口：注册插件、托盘、命令、全局状态——把各模块组装成完整应用
mod audio;
mod commands;
mod deliver;
mod config;
mod dictation;
mod engines;
mod focus;
mod history;
mod hotkey;
mod inject;
mod models;
mod panel;
mod perm;
mod state;
mod stt;
mod system;

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
            // 崩溃取证：panic 的位置与消息落盘（.ips 只给汇编栈不给 panic 消息；
            // dev 终端可能已滚走。2026-11-10 启动崩溃事后补的观测点）
            {
                let default_hook = std::panic::take_hook();
                std::panic::set_hook(Box::new(move |info| {
                    use std::io::Write;
                    if let Ok(mut f) = std::fs::OpenOptions::new()
                        .create(true)
                        .append(true)
                        .open("/tmp/sayit-panic.log")
                    {
                        let ts = std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .map(|d| d.as_secs())
                            .unwrap_or(0);
                        let _ = writeln!(f, "[{ts}] {info}");
                    }
                    default_hook(info);
                }));
            }

            // 托盘应用形态：不在 Dock 显示图标
            #[cfg(target_os = "macos")]
            app.set_activation_policy(tauri::ActivationPolicy::Accessory);

            // 面板窗口启动时摆好位（几何恒定，此后 show/hide 不再碰位置）
            panel::position_panel(app.handle());

            // 听写历史（设置页「历史」的数据源）
            history::load_history(app.handle());

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
            panel::hide_panel,
            panel::open_settings,
            system::mic_selftest,
            system::open_mic_settings,
            system::accessibility_status,
            system::copy_text,
            system::open_accessibility_settings,
            system::open_keyboard_settings,
            models::get_model_library,
            models::download_model,
            models::cancel_download,
            models::delete_model,
            models::add_user_model,
            models::set_default_model,
            history::get_history,
            history::delete_history,
            history::clear_history,
            hotkey::start_hotkey_capture,
            hotkey::stop_hotkey_capture,
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
