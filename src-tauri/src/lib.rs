use tauri::Manager;

// Learn more about Tauri commands at https://tauri.app/develop/calling-rust/

fn validate_username(username: &str) -> Result<(), String> {
    let trimmed = username.trim();
    if trimmed.is_empty() {
        return Err("Invalid username.".to_string());
    }
    if trimmed.len() > 256 {
        return Err("Invalid username.".to_string());
    }
    Ok(())
}

/// Lexically join `sub` under `base`, rejecting absolute paths, `~` escapes,
/// and `.` / `..` / prefix components. Returns the joined path.
fn join_contained(base: &std::path::Path, sub: &str) -> Result<std::path::PathBuf, String> {
    let trimmed = sub.trim();
    if trimmed.is_empty() {
        return Err("Empty subdirectory.".to_string());
    }
    // Reject Windows separators to keep component analysis sound.
    if trimmed.contains('\\') {
        return Err("Invalid subdirectory: backslashes are not allowed.".to_string());
    }
    let rel = std::path::Path::new(trimmed);
    if rel.is_absolute() {
        return Err("Invalid subdirectory: absolute paths are not allowed.".to_string());
    }
    for comp in rel.components() {
        match comp {
            std::path::Component::Normal(_) => {}
            // CurDir (`.`), ParentDir (`..`), RootDir, and Prefix are all rejected:
            // the mount target must be a plain relative descent under the base.
            _ => return Err("Invalid subdirectory: \".\" and \"..\" segments are not allowed.".to_string()),
        }
    }
    Ok(base.join(rel))
}

fn contained_string(base: &std::path::Path, joined: &std::path::Path) -> Result<String, String> {
    // Where possible, canonicalize both sides (resolves symlinks) and enforce containment.
    if let (Ok(cbase), Ok(cjoined)) = (base.canonicalize(), joined.canonicalize()) {
        if !cjoined.starts_with(&cbase) {
            return Err("Invalid subdirectory: escapes the allowed directory.".to_string());
        }
        return Ok(cjoined.to_string_lossy().to_string());
    }
    // Base may not exist yet (fresh profile): fall back to the lexically-checked join.
    Ok(joined.to_string_lossy().to_string())
}

#[tauri::command]
fn get_mount_dir(app: tauri::AppHandle, target_subdir: Option<String>) -> Result<String, String> {
    if let Some(subdir) = target_subdir {
        let trimmed = subdir.trim();
        if !trimmed.is_empty() {
            // `~` / `~/...` expands under the home directory, still contained.
            if trimmed == "~" || trimmed.starts_with("~/") {
                let home = app.path().home_dir().map_err(|_| "Unable to resolve home directory.".to_string())?;
                if trimmed == "~" {
                    return contained_string(&home, &home);
                }
                let remainder = &trimmed[2..];
                // `~//etc` yields an absolute remainder — reject instead of escaping home.
                if remainder.starts_with('/') {
                    return Err("Invalid subdirectory: absolute paths are not allowed.".to_string());
                }
                let joined = join_contained(&home, remainder)?;
                return contained_string(&home, &joined);
            }
            // Absolute paths are rejected: the mount target must live under home.
            if std::path::Path::new(trimmed).is_absolute() {
                return Err("Invalid subdirectory: absolute paths are not allowed.".to_string());
            }
            if let Ok(home) = app.path().home_dir() {
                let joined = join_contained(&home, trimmed)?;
                return contained_string(&home, &joined);
            }
        }
    }

    app.path()
        .document_dir()
        .map(|mut path| {
            path.push("scs-rclient");
            path.to_string_lossy().to_string()
        })
        .map_err(|_| "Unable to resolve the documents directory.".to_string())
}

#[tauri::command]
fn save_credentials(username: String, secret: String) -> Result<(), String> {
    validate_username(&username)?;
    let entry = keyring::Entry::new("scs-rclient", username.trim()).map_err(|e| {
        eprintln!("Keyring initialization failed: {}", e);
        "Failed to access secure storage.".to_string()
    })?;
    entry.set_password(&secret).map_err(|e| {
        eprintln!("Failed to save credentials in keyring: {}", e);
        "Failed to save credentials.".to_string()
    })?;
    Ok(())
}

#[tauri::command]
fn get_credentials(username: String) -> Result<String, String> {
    validate_username(&username)?;
    let entry = keyring::Entry::new("scs-rclient", username.trim()).map_err(|e| {
        eprintln!("Keyring initialization failed: {}", e);
        "Failed to access secure storage.".to_string()
    })?;
    entry.get_password().map_err(|e| {
        eprintln!("Failed to get credentials from keyring: {}", e);
        "No stored credentials found.".to_string()
    })
}

#[tauri::command]
fn delete_credentials(username: String) -> Result<(), String> {
    validate_username(&username)?;
    let entry = keyring::Entry::new("scs-rclient", username.trim()).map_err(|e| {
        eprintln!("Keyring initialization failed: {}", e);
        "Failed to access secure storage.".to_string()
    })?;
    match entry.delete_credential() {
        Ok(()) => Ok(()),
        Err(keyring::Error::NoEntry) => Ok(()),
        Err(e) => {
            eprintln!("Failed to delete credentials from keyring: {}", e);
            Err("Failed to delete credentials.".to_string())
        }
    }
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
            if let Ok(docs_dir) = app.path().document_dir() {
                let mut dir = docs_dir;
                dir.push("scs-rclient");
                // Refuse to follow a planted symlink at the mount parent.
                if let Ok(meta) = std::fs::symlink_metadata(&dir) {
                    if meta.file_type().is_symlink() {
                        eprintln!("Refusing to use symlinked scs-rclient directory: {}", dir.display());
                        return Ok(());
                    }
                }
                if let Err(e) = std::fs::create_dir_all(&dir) {
                    eprintln!("Failed to create scs-rclient directory: {}", e);
                }
                #[cfg(target_os = "linux")]
                {
                    use std::os::unix::fs::PermissionsExt;
                    if let Ok(meta) = std::fs::metadata(&dir) {
                        let mut perms = meta.permissions();
                        // Restrict the mount parent to the owner where it is group/other-readable.
                        if perms.mode() & 0o077 != 0 {
                            perms.set_mode(0o700);
                            if let Err(e) = std::fs::set_permissions(&dir, perms) {
                                eprintln!("Failed to restrict scs-rclient directory permissions: {}", e);
                            }
                        }
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
