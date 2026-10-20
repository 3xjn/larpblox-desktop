#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
mod bridge;
use serde_json::{json, Value};
use tauri::{
    menu::{Menu, MenuItem},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    Manager,
};
use tauri_plugin_autostart::ManagerExt;
use tauri_plugin_clipboard_manager::ClipboardExt;
use tauri_plugin_opener::OpenerExt;

#[tauri::command]
fn connection_status(app: tauri::AppHandle, state: tauri::State<bridge::Shared>) -> Value {
    let mut status = state.status();
    status["startup"] = json!(app.autolaunch().is_enabled().unwrap_or(false));
    status
}
#[tauri::command]
async fn retry_connection(state: tauri::State<'_, bridge::Shared>) -> Result<(), String> {
    bridge::serve(state.inner().clone()).await
}
#[tauri::command]
fn set_startup(app: tauri::AppHandle, enabled: bool) -> Result<(), String> {
    if enabled {
        app.autolaunch().enable()
    } else {
        app.autolaunch().disable()
    }
    .map_err(|e| e.to_string())
}
#[tauri::command]
fn minimize_to_tray(window: tauri::WebviewWindow) -> Result<(), String> {
    window.hide().map_err(|e| e.to_string())
}
#[tauri::command]
fn open_roblox(app: tauri::AppHandle) -> Result<(), String> {
    let chrome = ["PROGRAMFILES", "PROGRAMFILES(X86)", "LOCALAPPDATA"]
        .into_iter()
        .filter_map(std::env::var_os)
        .map(std::path::PathBuf::from)
        .map(|base| base.join("Google/Chrome/Application/chrome.exe"))
        .find(|path| path.is_file())
        .ok_or("Chrome is not installed. Install Chrome and the Larpblox extension first.")?;
    app.opener()
        .open_url(
            "https://www.roblox.com/my/avatar",
            Some(chrome.to_string_lossy()),
        )
        .map_err(|e| e.to_string())
}
#[tauri::command]
fn copy_launcher(app: tauri::AppHandle) -> Result<(), String> {
    app.clipboard().write_text("loadstring(game:HttpGet(\"https://raw.githubusercontent.com/noahkcarmstrong-ops/larpblox/main/launcher.lua\", false))()").map_err(|e|e.to_string())
}
fn main() {
    let bridge = bridge::Bridge::new().expect("Couldn't initialize local Larpblox data");
    tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _, _| {
            if let Some(w) = app.get_webview_window("main") {
                let _ = w.show();
                let _ = w.set_focus();
            }
        }))
        .plugin(
            tauri_plugin_autostart::Builder::new()
                .args(["--tray"])
                .build(),
        )
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_clipboard_manager::init())
        .manage(bridge.clone())
        .invoke_handler(tauri::generate_handler![
            connection_status,
            retry_connection,
            set_startup,
            minimize_to_tray,
            open_roblox,
            copy_launcher
        ])
        .setup(move |app| {
            let open = MenuItem::with_id(app, "open", "Open Larpblox", true, None::<&str>)?;
            let exit = MenuItem::with_id(app, "exit", "Exit", true, None::<&str>)?;
            let menu = Menu::with_items(app, &[&open, &exit])?;
            TrayIconBuilder::new()
                .icon(app.default_window_icon().unwrap().clone())
                .menu(&menu)
                .tooltip("Larpblox")
                .show_menu_on_left_click(false)
                .on_menu_event(|app, event| match event.id.as_ref() {
                    "open" => {
                        if let Some(w) = app.get_webview_window("main") {
                            let _ = w.show();
                            let _ = w.set_focus();
                        }
                    }
                    "exit" => app.exit(0),
                    _ => {}
                })
                .on_tray_icon_event(|tray, event| {
                    if let TrayIconEvent::Click {
                        button: MouseButton::Left,
                        button_state: MouseButtonState::Up,
                        ..
                    } = event
                    {
                        if let Some(w) = tray.app_handle().get_webview_window("main") {
                            let _ = w.show();
                            let _ = w.set_focus();
                        }
                    }
                })
                .build(app)?;
            if std::env::args().any(|a| a == "--tray") {
                if let Some(w) = app.get_webview_window("main") {
                    let _ = w.hide();
                }
            }
            tauri::async_runtime::spawn(bridge::serve(bridge.clone()));
            Ok(())
        })
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                api.prevent_close();
                let _ = window.hide();
            }
        })
        .run(tauri::generate_context!())
        .expect("Larpblox could not start");
}
