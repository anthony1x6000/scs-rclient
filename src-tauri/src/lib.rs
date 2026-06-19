use tauri::Manager;

// Learn more about Tauri commands at https://tauri.app/develop/calling-rust/
#[tauri::command]
fn greet(name: &str) -> String {
    format!("Hello, {}! You've been greeted from Rust!", name)
}

#[tauri::command]
fn get_mount_dir(app: tauri::AppHandle) -> Result<String, String> {
    app.path()
        .document_dir()
        .map(|mut path| {
            path.push("scs-rclient");
            path.to_string_lossy().to_string()
        })
        .map_err(|e| e.to_string())
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_store::Builder::default().build())
        .plugin(tauri_plugin_shell::init())
        .setup(|app| {
            // Ensure ~/Documents/scs-rclient exists on startup
            if let Ok(mut docs_dir) = app.path().document_dir() {
                docs_dir.push("scs-rclient");
                if !docs_dir.exists() {
                    if let Err(e) = std::fs::create_dir_all(&docs_dir) {
                        eprintln!("Failed to create scs-rclient directory: {}", e);
                    }
                }
            }
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![greet, get_mount_dir])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
