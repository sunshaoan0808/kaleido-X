//! Thin Kaleido desktop shell.
//!
//! Does **not** re-embed business logic. The window loads the Kaleido web
//! workbench from either:
//! - `KALEIDO_DESKTOP_URL` env (e.g. `https://kaleido.example.com/web/`)
//! - default local server `http://127.0.0.1:18766/web/`
//!
//! Process supervision for kaleido-server stays with systemd/operator so this
//! crate remains a pure WebView wrapper (D5).

use tauri::Manager;

fn start_url() -> String {
    std::env::var("KALEIDO_DESKTOP_URL")
        .unwrap_or_else(|_| "http://127.0.0.1:18766/web/?v=desktop".to_string())
}

#[tauri::command]
fn get_api_base() -> String {
    std::env::var("KALEIDO_API_BASE").unwrap_or_else(|_| "http://127.0.0.1:18766".into())
}

#[tauri::command]
fn get_start_url() -> String {
    start_url()
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_shell::init())
        .invoke_handler(tauri::generate_handler![get_api_base, get_start_url])
        .setup(|app| {
            let url = start_url();
            if let Some(win) = app.get_webview_window("main") {
                if let Ok(parsed) = url.parse() {
                    let _ = win.navigate(parsed);
                }
            }
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running Kaleido desktop");
}
