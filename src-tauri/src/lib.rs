use tauri::Manager;

// Learn more about Tauri commands at https://tauri.app/develop/calling-rust/
#[tauri::command]
fn get_mount_dir(app: tauri::AppHandle, target_subdir: Option<String>) -> Result<String, String> {
    if let Some(subdir) = target_subdir {
        let trimmed = subdir.trim();
        if !trimmed.is_empty() {
            let path = std::path::Path::new(trimmed);
            if path.is_absolute() {
                return Ok(path.to_string_lossy().to_string());
            }
            if trimmed.starts_with("~/") || trimmed == "~" {
                if let Ok(home) = app.path().home_dir() {
                    let mut p = home;
                    if trimmed.len() > 2 {
                        p.push(&trimmed[2..]);
                    }
                    return Ok(p.to_string_lossy().to_string());
                }
            }
            if let Ok(mut home) = app.path().home_dir() {
                home.push(trimmed);
                return Ok(home.to_string_lossy().to_string());
            }
        }
    }

    app.path()
        .document_dir()
        .map(|mut path| {
            path.push("scs-rclient");
            path.to_string_lossy().to_string()
        })
        .map_err(|e| e.to_string())
}

#[tauri::command]
fn save_credentials(username: String, secret: String) -> Result<(), String> {
    let entry = keyring::Entry::new("scs-rclient", &username).map_err(|e| {
        let err_msg = format!("Keyring initialization failed: {}", e);
        eprintln!("{}", err_msg);
        err_msg
    })?;
    entry.set_password(&secret).map_err(|e| {
        let err_msg = format!("Failed to save credentials in keyring: {}", e);
        eprintln!("{}", err_msg);
        err_msg
    })?;
    Ok(())
}

#[tauri::command]
fn get_credentials(username: String) -> Result<String, String> {
    let entry = keyring::Entry::new("scs-rclient", &username).map_err(|e| {
        let err_msg = format!("Keyring initialization failed: {}", e);
        eprintln!("{}", err_msg);
        err_msg
    })?;
    entry.get_password().map_err(|e| {
        let err_msg = format!("Failed to get credentials from keyring: {}", e);
        eprintln!("{}", err_msg);
        err_msg
    })
}

#[tauri::command]
fn delete_credentials(username: String) -> Result<(), String> {
    let entry = keyring::Entry::new("scs-rclient", &username).map_err(|e| {
        let err_msg = format!("Keyring initialization failed: {}", e);
        eprintln!("{}", err_msg);
        err_msg
    })?;
    let _ = entry.delete_credential();
    Ok(())
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    #[cfg(target_os = "linux")]
    {
        if std::env::var_os("WEBKIT_DISABLE_DMABUF_RENDERER").is_none() {
            std::env::set_var("WEBKIT_DISABLE_DMABUF_RENDERER", "1");
        }
    }

    tauri::Builder::default()
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
        .invoke_handler(tauri::generate_handler![
            get_mount_dir,
            save_credentials,
            get_credentials,
            delete_credentials
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
