use tauri::Manager;

fn is_executable_rclone(path: &std::path::Path) -> bool {
    if let Ok(output) = std::process::Command::new(path).arg("--version").output() {
        output.status.success()
    } else {
        false
    }
}

#[tauri::command]
fn resolve_rclone_binary(app: tauri::AppHandle) -> Result<String, String> {
    let mut search_dirs = Vec::new();

    if let Ok(res_dir) = app.path().resource_dir() {
        search_dirs.push(res_dir.clone());
        search_dirs.push(res_dir.join("binaries"));
    }

    if let Ok(exe_path) = std::env::current_exe() {
        if let Some(exe_dir) = exe_path.parent() {
            search_dirs.push(exe_dir.to_path_buf());
            search_dirs.push(exe_dir.join("binaries"));
            search_dirs.push(exe_dir.join("src-tauri").join("binaries"));
        }
    }

    if let Ok(cwd) = std::env::current_dir() {
        search_dirs.push(cwd.join("src-tauri").join("binaries"));
        search_dirs.push(cwd.join("binaries"));
    }

    for dir in &search_dirs {
        if let Ok(entries) = std::fs::read_dir(dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                    if name.starts_with("rclone-sidecar") {
                        if is_executable_rclone(&path) {
                            return Ok(path.to_string_lossy().to_string());
                        }
                    }
                }
            }
        }
    }

    let sys_cmd = if cfg!(target_os = "windows") { "rclone.exe" } else { "rclone" };
    if is_executable_rclone(std::path::Path::new(sys_cmd)) {
        return Ok(sys_cmd.to_string());
    }

    Err("Neither valid embedded rclone sidecar nor system 'rclone' was found on this system.".to_string())
}

#[cfg(target_os = "linux")]
fn spawn_linux_terminal(inner_cmd: &str) -> Result<(), String> {
    let candidate_terminals = vec![
        ("x-terminal-emulator", vec!["-e", "bash", "-c", inner_cmd]),
        ("gnome-terminal", vec!["--", "bash", "-c", inner_cmd]),
        ("konsole", vec!["-e", "bash", "-c", inner_cmd]),
        ("xfce4-terminal", vec!["-e", inner_cmd]),
        ("xterm", vec!["-e", "bash", "-c", inner_cmd]),
        ("alacritty", vec!["-e", "bash", "-c", inner_cmd]),
        ("kitty", vec!["bash", "-c", inner_cmd]),
    ];

    let mut last_err = String::new();
    for (term, args) in candidate_terminals {
        match std::process::Command::new(term).args(&args).spawn() {
            Ok(_) => return Ok(()),
            Err(e) => {
                last_err = format!("Failed to spawn {}: {}", term, e);
            }
        }
    }

    Err(format!(
        "No supported terminal emulator found on Linux. Last error: {}",
        last_err
    ))
}

#[tauri::command]
fn run_rclone_in_terminal(
    app: tauri::AppHandle,
    args: Vec<String>,
    env_pass: Option<String>,
) -> Result<String, String> {
    let rclone_binary = resolve_rclone_binary(app)?;

    #[cfg(target_os = "windows")]
    {
        let mut cmd_parts = Vec::new();
        cmd_parts.push(format!("\"{}\"", rclone_binary));
        for arg in &args {
            if arg.contains(' ') || arg.contains('&') || arg.contains('^') || arg.contains('%') {
                cmd_parts.push(format!("\"{}\"", arg));
            } else {
                cmd_parts.push(arg.clone());
            }
        }
        let rclone_cmd_str = cmd_parts.join(" ");

        let mut cmd = std::process::Command::new("cmd");
        cmd.args(["/c", "start", "scs-rclient rclone", "cmd", "/k", &rclone_cmd_str]);
        if let Some(ref pass) = env_pass {
            cmd.env("RCLONE_WEBDAV_PASS", pass);
        }
        cmd.spawn().map_err(|e| format!("Failed to launch terminal on Windows: {}", e))?;
    }

    #[cfg(target_os = "macos")]
    {
        let mut cmd_parts = Vec::new();
        cmd_parts.push(format!("\"{}\"", rclone_binary));
        for arg in &args {
            let escaped = arg.replace('\\', "\\\\").replace('"', "\\\"");
            cmd_parts.push(format!("\"{}\"", escaped));
        }
        let rclone_cmd_str = cmd_parts.join(" ");

        let script = if let Some(ref pass) = env_pass {
            let escaped_pass = pass.replace('\\', "\\\\").replace('"', "\\\"");
            format!(
                "tell application \"Terminal\"\n\
                 do script \"export RCLONE_WEBDAV_PASS=\\\"{}\\\"; {}; read -p \\\"Press Enter to close...\\\"\"\n\
                 activate\n\
                 end tell",
                escaped_pass, rclone_cmd_str
            )
        } else {
            format!(
                "tell application \"Terminal\"\n\
                 do script \"{}; read -p \\\"Press Enter to close...\\\"\"\n\
                 activate\n\
                 end tell",
                rclone_cmd_str
            )
        };

        std::process::Command::new("osascript")
            .arg("-e")
            .arg(&script)
            .spawn()
            .map_err(|e| format!("Failed to launch terminal on macOS: {}", e))?;
    }

    #[cfg(target_os = "linux")]
    {
        let mut cmd_parts = Vec::new();
        cmd_parts.push(format!("\"{}\"", rclone_binary));
        for arg in &args {
            let escaped = arg.replace('\\', "\\\\").replace('"', "\\\"");
            cmd_parts.push(format!("\"{}\"", escaped));
        }
        let rclone_cmd_str = cmd_parts.join(" ");

        let inner_cmd = if let Some(ref pass) = env_pass {
            let escaped_pass = pass.replace('\\', "\\\\").replace('"', "\\\"");
            format!(
                "export RCLONE_WEBDAV_PASS=\"{}\"; {}; echo ''; read -p 'Press Enter to close...'",
                escaped_pass, rclone_cmd_str
            )
        } else {
            format!(
                "{}; echo ''; read -p 'Press Enter to close...'",
                rclone_cmd_str
            )
        };

        spawn_linux_terminal(&inner_cmd)?;
    }

    Ok(format!("Terminal opened using rclone binary: {}", rclone_binary))
}

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
            delete_credentials,
            resolve_rclone_binary,
            run_rclone_in_terminal
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

