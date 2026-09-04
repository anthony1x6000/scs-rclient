use tauri::Manager;

// Learn more about Tauri commands at https://tauri.app/develop/calling-rust/
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

use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;

fn get_fallback_credentials_path(app: &tauri::AppHandle) -> Result<PathBuf, String> {
    let mut dir = app
        .path()
        .app_data_dir()
        .map_err(|e| format!("Failed to get app data directory: {}", e))?;

    if !dir.exists() {
        fs::create_dir_all(&dir).map_err(|e| format!("Failed to create app data directory: {}", e))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = fs::set_permissions(&dir, fs::Permissions::from_mode(0o700));
        }
    }
    dir.push("credentials.json");
    Ok(dir)
}

fn load_fallback_credentials(app: &tauri::AppHandle) -> HashMap<String, String> {
    if let Ok(path) = get_fallback_credentials_path(app) {
        if path.exists() {
            if let Ok(content) = fs::read_to_string(&path) {
                if let Ok(map) = serde_json::from_str::<HashMap<String, String>>(&content) {
                    return map;
                }
            }
        }
    }
    HashMap::new()
}

fn save_fallback_credentials(app: &tauri::AppHandle, username: &str, secret: &str) -> Result<(), String> {
    let path = get_fallback_credentials_path(app)?;
    let mut map = load_fallback_credentials(app);
    map.insert(username.to_string(), secret.to_string());

    let json = serde_json::to_string_pretty(&map).map_err(|e| e.to_string())?;
    fs::write(&path, json).map_err(|e| format!("Failed to write fallback credentials: {}", e))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(&path, fs::Permissions::from_mode(0o600));
    }
    Ok(())
}

fn delete_fallback_credentials(app: &tauri::AppHandle, username: &str) -> Result<(), String> {
    if let Ok(path) = get_fallback_credentials_path(app) {
        if path.exists() {
            let mut map = load_fallback_credentials(app);
            if map.remove(username).is_some() {
                let json = serde_json::to_string_pretty(&map).map_err(|e| e.to_string())?;
                let _ = fs::write(&path, json);
            }
        }
    }
    Ok(())
}

#[tauri::command]
fn save_credentials(app: tauri::AppHandle, username: String, secret: String) -> Result<(), String> {
    let mut keyring_saved = false;
    if let Ok(entry) = keyring::Entry::new("scs-rclient", &username) {
        if let Err(e) = entry.set_password(&secret) {
            eprintln!(
                "Keyring save failed ({}), falling back to app secure credential storage.",
                e
            );
        } else {
            keyring_saved = true;
            let _ = delete_fallback_credentials(&app, &username);
        }
    } else {
        eprintln!("Keyring initialization failed, falling back to app secure credential storage.");
    }

    if !keyring_saved {
        save_fallback_credentials(&app, &username, &secret)?;
    }
    Ok(())
}

#[tauri::command]
fn get_credentials(app: tauri::AppHandle, username: String) -> Result<String, String> {
    if let Ok(entry) = keyring::Entry::new("scs-rclient", &username) {
        if let Ok(pass) = entry.get_password() {
            return Ok(pass);
        }
    }

    let map = load_fallback_credentials(&app);
    if let Some(pass) = map.get(&username) {
        return Ok(pass.clone());
    }

    Err(format!("No credentials found for user: {}", username))
}

#[tauri::command]
fn delete_credentials(app: tauri::AppHandle, username: String) -> Result<(), String> {
    if let Ok(entry) = keyring::Entry::new("scs-rclient", &username) {
        let _ = entry.delete_credential();
    }
    delete_fallback_credentials(&app, &username)
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
