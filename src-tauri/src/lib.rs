#![deny(unsafe_op_in_unsafe_fn)]

use std::io::Read;
use tauri::Manager;

mod timefmt;

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

/// Opens `dir` in the platform's default file manager.
fn open_in_file_manager(dir: &std::path::Path) -> Result<(), String> {
    #[cfg(target_os = "windows")]
    let opener = "explorer";
    #[cfg(target_os = "macos")]
    let opener = "open";
    #[cfg(all(unix, not(target_os = "macos")))]
    let opener = "xdg-open";

    std::process::Command::new(opener)
        .arg(dir)
        .spawn()
        .map(|_| ())
        .map_err(|e| format!("Failed to open {}: {}", dir.display(), e))
}

/// Opens the local working directory (mount dir + selected subdir) in the file manager.
/// The path is resolved with the same containment rules as WebDAV actions.
#[tauri::command]
fn open_local_dir(
    app: tauri::AppHandle,
    target_subdir: Option<String>,
    subdir: String,
) -> Result<(), String> {
    let mount_dir = get_mount_dir(app, target_subdir)?;
    let base = std::path::Path::new(&mount_dir);
    let trimmed = subdir.trim();
    let dir = if trimmed.is_empty() || trimmed == "." {
        base.to_path_buf()
    } else {
        join_contained(base, trimmed)?
    };
    std::fs::create_dir_all(&dir).map_err(|e| format!("Failed to create {}: {}", dir.display(), e))?;
    open_in_file_manager(&dir)
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

pub mod dav_client;
pub mod webdav;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

pub struct WebdavState {
    pub cancel_flag: Arc<AtomicBool>,
}

#[tauri::command]
fn verify_webdav(
    base_url: String,
    subdir: String,
    username: String,
    password: Option<String>,
) -> Result<(), String> {
    let secret = match password {
        Some(p) => p,
        None => get_credentials(username.clone())?,
    };
    let remote_url = webdav::build_remote_url(&base_url, &subdir);
    webdav::validate_webdav_url(&remote_url)?;
    webdav::verify_webdav_auth(&remote_url, &username, &secret)
}

#[tauri::command]
fn cancel_webdav_action(state: tauri::State<'_, WebdavState>) -> Result<(), String> {
    state.cancel_flag.store(true, Ordering::SeqCst);
    // Parked workers wait on a condvar without polling; wake them so they observe it.
    webdav::wake_worker_pools();
    Ok(())
}

/// Applies a new worker thread limit to scans and uploads that are already running.
#[tauri::command]
fn set_scan_concurrency(threads: usize) -> Result<(), String> {
    webdav::set_scan_concurrency(threads);
    Ok(())
}

/// Frontend wire format for one progress snapshot. The event name is
/// `webdav-progress`; fields are camelCase so the React hook consumes them
/// without a mapping layer.
#[derive(Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct WebdavProgressPayload {
    subdir: String,
    action: String,
    phase: String,
    completed: usize,
    total: usize,
    bytes_done: u64,
    bytes_total: u64,
    file: Option<String>,
}

/// Frontend wire format for an extension filter: `{ mode: "whitelist"|"blacklist", extensions: [...] }`.
#[derive(serde::Deserialize)]
struct ExtensionFilterArgs {
    mode: String,
    extensions: Vec<String>,
}

impl ExtensionFilterArgs {
    /// Normalizes into a backend filter, failing closed on anything malformed: an
    /// unknown mode or a list with no valid entries is an error rather than a
    /// silently dropped filter, so a whitelist intent can never degrade into
    /// transferring everything.
    fn into_filter(self) -> Result<webdav::ExtensionFilter, String> {
        let mode = webdav::ExtensionFilterMode::parse(&self.mode)
            .ok_or_else(|| format!("Invalid extension filter mode: \"{}\".", self.mode))?;
        let filter = webdav::ExtensionFilter::new(mode, &self.extensions);
        if !filter.is_active() {
            return Err("Extension filter has no valid entries.".to_string());
        }
        Ok(filter)
    }
}

/// Runs a native WebDAV action (e.g. "put", "get", "sync", "list").
///
/// Parameters:
/// - `concurrency`: Optional number of worker threads for parallel remote directory traversal and concurrent uploads (1..64).
///   When `None`, the current live limit is used (the environment default until changed).
///   `set_scan_concurrency` resizes the pools while the action is still running.
/// - `username`: Optional authenticated username, used for credential-scoped cache partitioning.
/// - `backup`: When true, the local side is redirected under `<mount>/backups`, keeping an
///   operation off the working copy it is meant to leave alone.
/// - `backup_dated`: When true (and `backup` is also true), the redirect is nested one level
///   deeper under today's date, giving each day its own `<mount>/backups/<YYYY-MM-DD>` folder.
/// - `exclude_folders`: Relative folder patterns (e.g. `.git/`) that scanning and transfers
///   must skip entirely — they are never traversed, uploaded, downloaded, or deleted by sync.
/// - `extension_filter`: Optional [`ExtensionFilterArgs`] that gates files by extension on
///   both sides of every scan. An unknown mode or an entry list with no valid extensions
///   fails the action instead of silently disabling the filter.
#[tauri::command]
async fn run_webdav_action(
    app: tauri::AppHandle,
    state: tauri::State<'_, WebdavState>,
    action: String,
    base_url: String,
    subdir: String,
    target_subdir: Option<String>,
    username: Option<String>,
    concurrency: Option<usize>,
    backup: Option<bool>,
    backup_dated: Option<bool>,
    exclude_folders: Option<Vec<String>>,
    extension_filter: Option<ExtensionFilterArgs>,
) -> Result<(), String> {
    use tauri::Emitter;

    state.cancel_flag.store(false, Ordering::SeqCst);
    let cancel_flag = state.cancel_flag.clone();
    let exclude_folders = webdav::normalize_exclude_folders(&exclude_folders.unwrap_or_default());
    let extension_filter = extension_filter.map(ExtensionFilterArgs::into_filter).transpose()?;

    let mount_dir_str = get_mount_dir(app.clone(), target_subdir)?;
    let base_mount = std::path::Path::new(&mount_dir_str);
    let dest_root = if backup.unwrap_or(false) {
        let backups = join_contained(base_mount, "backups")?;
        if backup_dated.unwrap_or(false) {
            let today = crate::timefmt::local_date_ymd();
            join_contained(&backups, &today)?
        } else {
            backups
        }
    } else {
        base_mount.to_path_buf()
    };
    let trimmed_subdir = subdir.trim();
    let local_path = if trimmed_subdir.is_empty() || trimmed_subdir == "." {
        dest_root
    } else {
        join_contained(&dest_root, trimmed_subdir)?
    };

    let remote_url = webdav::build_remote_url(&base_url, &subdir);
    webdav::validate_webdav_url(&remote_url)?;

    let (user, pass) = if let Some(u) = username.filter(|u| !u.trim().is_empty()) {
        let p = get_credentials(u.clone()).unwrap_or_default();
        (u, p)
    } else {
        (String::new(), String::new())
    };

    let auth_user = if user.is_empty() {
        None
    } else {
        Some(user.clone())
    };

    let client = dav_client::Client::init(&user, &pass, &remote_url)?;
    let app_handle = app.clone();
    let progress_app = app.clone();
    let progress_subdir = subdir.clone();

    tauri::async_runtime::spawn_blocking(move || {
        let emit_log = move |msg: &str| {
            let _ = app_handle.emit("webdav-log", msg);
        };
        // Transfers can report thousands of chunks; cap event traffic at ~10/s
        // so IPC stays cheap while the bar still moves smoothly.
        let started = std::time::Instant::now();
        let last_emit = std::sync::atomic::AtomicU64::new(0);
        let subdir_for_progress = progress_subdir;
        let emit_progress = move |p: webdav::ActionProgress| {
            let elapsed = started.elapsed().as_millis() as u64;
            let previous = last_emit.load(Ordering::Relaxed);
            // Per-file summaries (file is None once a file finishes) are rare
            // and carry the counts the UI leaves on screen, so never drop them.
            let summary = p.file.is_none() && p.completed > 0;
            if !summary && elapsed.saturating_sub(previous) < 100 {
                return;
            }
            last_emit.store(elapsed, Ordering::Relaxed);
            let _ = progress_app.emit(
                "webdav-progress",
                WebdavProgressPayload {
                    subdir: subdir_for_progress.clone(),
                    action: p.action,
                    phase: p.phase.to_string(),
                    completed: p.completed,
                    total: p.total,
                    bytes_done: p.bytes_done,
                    bytes_total: p.bytes_total,
                    file: p.file,
                },
            );
        };
        webdav::execute_webdav_action_with_progress(
            &client,
            &action,
            &remote_url,
            &local_path,
            &cancel_flag,
            concurrency,
            auth_user.as_deref(),
            &exclude_folders,
            extension_filter.as_ref(),
            emit_progress,
            emit_log,
        )
    })
    .await
    .map_err(|e| format!("Task execution error: {}", e))?
}

/// Clears the WebDAV remote directory listing cache.
/// Returns Ok(()) if the cache was deleted or did not exist.
#[tauri::command]
fn clear_webdav_cache() -> Result<(), String> {
    webdav::clear_remote_cache()
}

/// The Tauri store file that holds the WebDAV base URL and saved username.
const SETTINGS_STORE_FILE: &str = "settings.json";

fn stored_setting(app: &tauri::AppHandle, key: &str) -> Option<String> {
    // `store()` loads-or-creates, so the command does not depend on the
    // renderer having registered the store first.
    tauri_plugin_store::StoreExt::store(app, SETTINGS_STORE_FILE)
        .ok()
        .and_then(|store| store.get(key))
        .and_then(|value| value.get("value").and_then(|v| v.as_str()).map(str::to_string))
        .filter(|v| !v.trim().is_empty())
}

/// Loads the authenticated WebDAV origin from the settings store + keyring.
/// A username with no keyring entry fails loudly instead of authenticating
/// with an empty password.
fn load_stored_webdav(app: &tauri::AppHandle) -> Result<(String, String, String), String> {
    let base_url = stored_setting(app, "webdav_url")
        .ok_or("WebDAV URL is not configured. Set it in Settings first.")?;
    let username = stored_setting(app, "saved_username")
        .ok_or("No saved username. Save your credentials first.")?;
    let password = get_credentials(username.clone())?;
    Ok((base_url, username, password))
}

/// Rejects a derived remote URL whose scheme/host/port differs from the stored
/// base, so a credentialed request can never leave the configured origin.
fn ensure_same_origin(base_url: &str, remote_url: &str) -> Result<(), String> {
    let (base_origin, remote_origin) = (
        reqwest::Url::parse(base_url).map_err(|e| format!("Invalid stored WebDAV URL: {e}"))?,
        reqwest::Url::parse(remote_url).map_err(|e| format!("Invalid remote URL: {e}"))?,
    );
    if base_origin.scheme() != remote_origin.scheme()
        || base_origin.host_str() != remote_origin.host_str()
        || base_origin.port() != remote_origin.port()
    {
        return Err("Resolved remote URL leaves the configured WebDAV origin.".to_string());
    }
    Ok(())
}

/// Reads one local preview side, capped at [`webdav::MAX_PREVIEW_BYTES`].
fn preview_local_side(path: &std::path::Path) -> Result<webdav::PreviewSide, String> {
    let meta = match std::fs::symlink_metadata(path) {
        Ok(meta) if meta.is_file() => meta,
        Ok(_) => return Ok(webdav::PreviewSide::missing()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Ok(webdav::PreviewSide::missing())
        }
        Err(e) => return Err(format!("Could not inspect {}: {e}", path.display())),
    };
    let mut file = std::fs::File::open(path)
        .map_err(|e| format!("Could not open {}: {e}", path.display()))?;
    let mut bytes = Vec::new();
    file.take(webdav::MAX_PREVIEW_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| format!("Could not read {}: {e}", path.display()))?;
    Ok(webdav::preview_side_from_bytes(
        &bytes,
        meta.len(),
        webdav::get_metadata_mtime(&meta),
    ))
}

/// Reads one remote preview side, capped at [`webdav::MAX_PREVIEW_BYTES`].
/// Redirects are disabled by the client, so the URL that was origin-checked is
/// the only host this GET can reach.
fn preview_remote_side(
    client: &dav_client::Client,
    url: &str,
) -> Result<webdav::PreviewSide, String> {
    let mut response = client
        .get(url)
        .map_err(|e| format!("GET {url} failed: {e}"))?;
    let status = response.status();
    if status == reqwest::StatusCode::NOT_FOUND || status == reqwest::StatusCode::GONE {
        return Ok(webdav::PreviewSide::missing());
    }
    if !status.is_success() {
        return Err(format!("GET {url} returned HTTP {status}"));
    }
    let mtime = response
        .headers()
        .get("last-modified")
        .and_then(|value| value.to_str().ok())
        .and_then(webdav::parse_webdav_date);
    let advertised = response.content_length();
    let mut bytes = Vec::new();
    response
        .take(webdav::MAX_PREVIEW_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| format!("Could not read {url}: {e}"))?;
    let size = advertised.unwrap_or(bytes.len() as u64);
    Ok(webdav::preview_side_from_bytes(&bytes, size, mtime))
}

/// Lists every folder beneath the remote directory `subdir`, each with the total
/// size of the files beneath it (PROPFIND metadata only — no downloads).
///
/// Powers the "Exclude folder select" picker. Uses its own cancel flag so
/// opening the picker never resets cancellation of a running action.
///
/// Hardening: the authenticated origin and identity are backend-owned — both
/// come from the saved settings store, renderer input is never accepted for
/// them, and missing or malformed stored auth state is an error rather than a
/// fallback. Traversal segments in `subdir` (literal or percent-encoded) are
/// rejected because `build_remote_url` is a plain join.
#[tauri::command]
async fn list_remote_folders(
    app: tauri::AppHandle,
    subdir: String,
    concurrency: Option<usize>,
) -> Result<Vec<webdav::RemoteFolder>, String> {
    let (base_url, username, pass) = load_stored_webdav(&app)?;
    let concurrency = concurrency.map(|c| c.clamp(1, webdav::MAX_SCAN_CONCURRENCY));

    if webdav::has_traversal_segments(&subdir) {
        return Err("Invalid subdirectory: \"..\" segments are not allowed.".to_string());
    }

    let remote_url = webdav::build_remote_url(&base_url, &subdir);
    webdav::validate_webdav_url(&remote_url)?;
    ensure_same_origin(&base_url, &remote_url)?;

    let client = dav_client::Client::init(&username, &pass, &remote_url)?;
    let cancel_flag = Arc::new(AtomicBool::new(false));

    tauri::async_runtime::spawn_blocking(move || {
        webdav::list_remote_folders(
            &client,
            &remote_url,
            &cancel_flag,
            concurrency,
            Some(username.as_str()),
            |_| {},
        )
    })
    .await
    .map_err(|e| format!("Task execution error: {}", e))?
}

/// Computes the changed-file list behind the Review Changes view: one walk of
/// the local mount and one fresh recursive listing of the remote collection,
/// both filtered by the same exclusions and extension filter as a transfer.
/// Rows describe the upload: a file that exists only in the working copy is
/// `added` (it will be uploaded), not a remote removal.
///
/// Returns metadata only — file bodies are fetched lazily by
/// [`read_change_preview`] for the row the user clicks.
#[tauri::command]
async fn review_changes(
    app: tauri::AppHandle,
    subdir: String,
    target_subdir: Option<String>,
    concurrency: Option<usize>,
    exclude_folders: Option<Vec<String>>,
    extension_filter: Option<ExtensionFilterArgs>,
) -> Result<Vec<webdav::ChangeEntry>, String> {
    let (base_url, username, pass) = load_stored_webdav(&app)?;
    let concurrency = concurrency.map(|c| c.clamp(1, webdav::MAX_SCAN_CONCURRENCY));

    if webdav::has_traversal_segments(&subdir) {
        return Err("Invalid subdirectory: \"..\" segments are not allowed.".to_string());
    }

    let remote_url = webdav::build_remote_url(&base_url, &subdir);
    webdav::validate_webdav_url(&remote_url)?;
    ensure_same_origin(&base_url, &remote_url)?;

    let exclude_folders = webdav::normalize_exclude_folders(&exclude_folders.unwrap_or_default());
    let extension_filter = extension_filter.map(ExtensionFilterArgs::into_filter).transpose()?;

    let mount_dir = get_mount_dir(app.clone(), target_subdir)?;
    let local_dir = if subdir.trim().is_empty() {
        std::path::PathBuf::from(mount_dir)
    } else {
        join_contained(std::path::Path::new(&mount_dir), &subdir)?
    };

    let client = dav_client::Client::init(&username, &pass, &remote_url)?;
    let cancel_flag = Arc::new(AtomicBool::new(false));

    tauri::async_runtime::spawn_blocking(move || {
        let items = webdav::list_remote_recursive_with_exclusions_and_log(
            &client,
            &remote_url,
            &cancel_flag,
            concurrency,
            Some(username.as_str()),
            false,
            &exclude_folders,
            extension_filter.as_ref(),
            |_| {},
        )?;
        let local_files = webdav::collect_local_files_with_mtime_excluding(
            &local_dir,
            &exclude_folders,
            extension_filter.as_ref(),
        );
        Ok::<_, String>(webdav::compute_change_entries(
            &remote_url,
            &items,
            &local_files,
        ))
    })
    .await
    .map_err(|e| format!("Task execution error: {}", e))?
}

/// Fetches both sides of one changed file for the diff preview. Text bodies are
/// capped at [`webdav::MAX_PREVIEW_BYTES`]; larger or binary files come back as
/// metadata-only sides. Paths are re-validated here rather than trusting the
/// review rows from the renderer.
#[tauri::command]
async fn read_change_preview(
    app: tauri::AppHandle,
    subdir: String,
    rel_path: String,
    target_subdir: Option<String>,
) -> Result<webdav::ChangePreview, String> {
    if webdav::has_traversal_segments(&subdir) {
        return Err("Invalid subdirectory: \"..\" segments are not allowed.".to_string());
    }
    if webdav::has_traversal_segments(&rel_path) || !webdav::is_safe_relative_path(&rel_path) {
        return Err("Invalid file path.".to_string());
    }

    let (base_url, username, pass) = load_stored_webdav(&app)?;
    let remote_url = webdav::build_remote_url(&base_url, &subdir);
    webdav::validate_webdav_url(&remote_url)?;
    ensure_same_origin(&base_url, &remote_url)?;

    let file_url = webdav::build_file_url(&remote_url, &rel_path);
    webdav::validate_webdav_url(&file_url)?;
    ensure_same_origin(&base_url, &file_url)?;

    let mount_dir = get_mount_dir(app.clone(), target_subdir)?;
    let local_root = if subdir.trim().is_empty() {
        std::path::PathBuf::from(mount_dir)
    } else {
        join_contained(std::path::Path::new(&mount_dir), &subdir)?
    };
    let local_path = webdav::safe_join_path(&local_root, &rel_path)?;

    let client = dav_client::Client::init(&username, &pass, &remote_url)?;

    tauri::async_runtime::spawn_blocking(move || {
        let local = preview_local_side(&local_path)?;
        let remote = preview_remote_side(&client, &file_url)?;
        Ok::<_, String>(webdav::ChangePreview {
            path: rel_path,
            local,
            remote,
        })
    })
    .await
    .map_err(|e| format!("Task execution error: {}", e))?
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    #[cfg(target_os = "linux")]
    {
        if std::env::var_os("WEBKIT_DISABLE_DMABUF_RENDERER").is_none() {
            std::env::set_var("WEBKIT_DISABLE_DMABUF_RENDERER", "1");
        }
    }

    let cancel_flag = Arc::new(AtomicBool::new(false));

    tauri::Builder::default()
        .manage(WebdavState { cancel_flag })
        .plugin(tauri_plugin_store::Builder::default().build())
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
            open_local_dir,
            save_credentials,
            get_credentials,
            delete_credentials,
            verify_webdav,
            cancel_webdav_action,
            set_scan_concurrency,
            run_webdav_action,
            list_remote_folders,
            review_changes,
            read_change_preview,
            clear_webdav_cache
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

