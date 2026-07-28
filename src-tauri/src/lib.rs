use tauri::Manager;
use tauri::window::Color;

#[tauri::command]
fn greet(name: &str) -> String {
    format!("Hello, {}! You've been greeted from Rust!", name)
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .invoke_handler(tauri::generate_handler![greet])
        .setup(|app| {
            // Force the webview background to fully transparent on Linux/Wayland
            let window = app.get_webview_window("main").unwrap();

            // Set the window background color to fully transparent RGBA
            // This is critical for WebKitGTK on Wayland where `transparent: true`
            // in tauri.conf.json alone is not sufficient
            let _ = window.set_background_color(Some(Color(0, 0, 0, 0)));

            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
