use std::collections::{HashMap, HashSet, VecDeque};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Condvar, Mutex};
use std::time::Duration;

pub const DEFAULT_MAX_FILE_SIZE: u64 = 500 * 1024 * 1024; // 500 MB
/// Default concurrency level for remote Depth: 1 folder scanning.
pub const DEFAULT_SCAN_CONCURRENCY: usize = 6;
/// Maximum concurrency level for remote Depth: 1 folder scanning to prevent overwhelming the server.
pub const MAX_SCAN_CONCURRENCY: usize = 64;
/// Safety limit on the number of traversed directories to guard against recursive symlink bombs or infinite trees.
/// Set to 10,000 to comfortably accommodate very large course hierarchies (typical max depth ~10 * breadth ~100)
/// while bounding memory usage and avoiding infinite traversal cycles.
pub const MAX_SCANNED_DIRS_LIMIT: usize = 10_000;

// Static compile-time assertion verifying that rustydav::client::Client implements Send + Sync
// and can safely be shared across concurrent scanning worker threads.
// Note: rustydav::client::Client internally wraps reqwest::blocking::Client, which maintains
// an Arc-backed connection pool designed for concurrent multi-threaded usage.
const _: fn() = || {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<rustydav::client::Client>();
};

/// Returns the concurrency limit for remote WebDAV folder scanning.
pub fn get_scan_concurrency() -> usize {
    std::env::var("WEBDAV_SCAN_CONCURRENCY")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .map(|v| v.clamp(1, MAX_SCAN_CONCURRENCY))
        .unwrap_or(DEFAULT_SCAN_CONCURRENCY)
}

/// Retrieves maximum allowed WebDAV file size in bytes, configurable via MAX_WEBDAV_FILE_SIZE_BYTES.
pub fn get_max_file_size() -> u64 {
    std::env::var("MAX_WEBDAV_FILE_SIZE_BYTES")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(DEFAULT_MAX_FILE_SIZE)
}

/// Computes SHA256 checksum of a file on disk by streaming chunks to avoid buffering large files in RAM.
pub fn compute_file_sha256(path: &Path) -> Result<String, std::io::Error> {
    use sha2::{Digest, Sha256};
    let mut file = std::fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = [0u8; 65536];
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        hasher.update(&buffer[..count]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub struct WebdavItem {
    pub href: String,
    pub is_dir: bool,
    pub size: u64,
    pub mtime: Option<u64>,
}

impl WebdavItem {
    /// Creates a new WebdavItem with optional mtime defaulted to None for backwards compatibility.
    pub fn new(href: impl Into<String>, is_dir: bool, size: u64) -> Self {
        Self {
            href: href.into(),
            is_dir,
            size,
            mtime: None,
        }
    }

    /// Sets the optional mtime on WebdavItem.
    pub fn with_mtime(mut self, mtime: Option<u64>) -> Self {
        self.mtime = mtime;
        self
    }
}

impl Default for WebdavItem {
    fn default() -> Self {
        Self {
            href: String::new(),
            is_dir: false,
            size: 0,
            mtime: None,
        }
    }
}

/// Returns true if a path or href contains directory traversal sequences or forbidden characters.
/// Allows legitimate hidden files (e.g. `.gitignore`, `.env`, `.github`) while strictly blocking
/// directory traversal attacks (`..`, `../`, `..\`, `/..`, `\..`, `%2e%2e`).
pub fn has_traversal_sequence(s: &str) -> bool {
    if s.contains('\\') || s.contains('\0') || s.contains('\r') || s.contains('\n') {
        return true;
    }
    if s == ".." || s.starts_with("../") || s.ends_with("/..") || s.contains("/../") {
        return true;
    }
    let lower = s.to_ascii_lowercase();
    if lower.contains("%2e%2e") || lower.contains("%2f..") || lower.contains("..%2f") {
        return true;
    }
    for part in s.split('/') {
        if part.trim() == ".." {
            return true;
        }
    }
    false
}

/// Computes a normalized cache key partitioned by authenticated credential context.
///
/// Multi-Account Isolation:
/// Cache keys include the authenticated username when available (`username@clean_url`) to prevent
/// cache collisions across different user credentials accessing the same WebDAV host (CWE-287 / CWE-384).
pub fn cache_key(remote_url: &str, auth_user: Option<&str>) -> String {
    let clean_url = remote_url.trim_end_matches('/').to_ascii_lowercase();
    match auth_user {
        Some(user) if !user.trim().is_empty() => {
            format!("{}@{}", user.trim().to_ascii_lowercase(), clean_url)
        }
        _ => clean_url,
    }
}

/// Represents a cached remote WebDAV directory listing.
/// Note: Cached listings are keyed by normalized collection URL and authenticated user.
/// If switching credentials or access permissions for the same URL, listings are safely partitioned,
/// or invoke `clear_remote_cache()` / click 'Clear Cache' under Settings to invalidate prior cached listings.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct CachedRemoteListing {
    pub remote_url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auth_user: Option<String>,
    pub timestamp: u64,
    pub items: Vec<WebdavItem>,
}

/// Default TTL for cached remote file listings (24 hours).
pub const DEFAULT_CACHE_TTL_SECS: u64 = 86400;

/// Returns whether remote WebDAV file caching is enabled.
pub fn is_cache_enabled() -> bool {
    if cfg!(test) || std::env::var("TEST_WEBDAV_URL").is_ok() {
        return std::env::var("ENABLE_WEBDAV_CACHE_IN_TEST").as_deref() == Ok("1");
    }
    std::env::var("WEBDAV_CACHE_DISABLED").as_deref() != Ok("1")
}

/// Returns the cache time-to-live in seconds.
pub fn get_cache_ttl_secs() -> u64 {
    std::env::var("WEBDAV_CACHE_TTL_SECS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(DEFAULT_CACHE_TTL_SECS)
}

/// Returns the path to the WebDAV remote listing cache file if a secure user directory is available.
///
/// Security & Access Control:
/// - Unix/Linux/macOS: The cache file is located inside `$XDG_CACHE_HOME` or `$HOME/.cache`, with
///   parent directory created mode `0o700` and file written mode `0o600` (CWE-200 / CWE-732).
/// - Windows Limitation: Fine-grained per-file DACLs are not manipulated via Win32 API.
///   Access control relies on the container DACL (`%LOCALAPPDATA%` or `%APPDATA%`), which natively
///   restricts access to the current user profile and SYSTEM.
/// - Fallback Rejection: If no user home or AppData directory is resolvable, fallback to shared/world-readable
///   temporary directories (/tmp) is strictly rejected (returns `None`), safely disabling disk caching.
pub fn get_cache_file_path() -> Option<PathBuf> {
    if let Ok(custom) = std::env::var("WEBDAV_CACHE_FILE") {
        let p = PathBuf::from(custom);
        if !p.as_os_str().is_empty() {
            return Some(p);
        }
    }

    #[cfg(unix)]
    {
        if let Ok(xdg) = std::env::var("XDG_CACHE_HOME") {
            let p = PathBuf::from(xdg);
            if p.is_absolute() {
                return Some(p.join("scs-rclient").join("webdav_cache.json"));
            }
        }
        if let Ok(home) = std::env::var("HOME") {
            let p = PathBuf::from(home);
            if p.is_absolute() {
                return Some(p.join(".cache").join("scs-rclient").join("webdav_cache.json"));
            }
        }
    }

    #[cfg(windows)]
    {
        if let Ok(local_appdata) = std::env::var("LOCALAPPDATA") {
            let p = PathBuf::from(local_appdata);
            if !p.as_os_str().is_empty() {
                return Some(p.join("scs-rclient").join("webdav_cache.json"));
            }
        }
        if let Ok(appdata) = std::env::var("APPDATA") {
            let p = PathBuf::from(appdata);
            if !p.as_os_str().is_empty() {
                return Some(p.join("scs-rclient").join("webdav_cache.json"));
            }
        }
    }

    None
}

/// Atomically writes content to the cache file using a temporary file and atomic rename.
///
/// Security & Access Control:
/// - Unix/Linux/macOS: The parent directory is created with mode `0o700` and the temporary file
///   with mode `0o600` before atomic rename, ensuring multi-user isolation on shared systems.
/// - Windows Limitation: Fine-grained per-file DACLs are not manipulated via Win32 API.
///   Security isolation relies on the enclosing parent folder's DACL (e.g. `%LOCALAPPDATA%`).
fn write_cache_atomic(path: &Path, content: &str) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            let mut builder = std::fs::DirBuilder::new();
            builder.recursive(true);
            builder.mode(0o700);
            let _ = builder.create(parent);
        }
        #[cfg(not(unix))]
        {
            let _ = std::fs::create_dir_all(parent);
        }
    }

    if let Ok(meta) = std::fs::symlink_metadata(path) {
        if meta.file_type().is_symlink() {
            let _ = std::fs::remove_file(path);
        }
    }

    let tmp_path = path.with_extension(format!("tmp.{}", std::process::id()));
    #[cfg(unix)]
    {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&tmp_path)?;
        file.write_all(content.as_bytes())?;
        file.sync_all()?;
    }
    #[cfg(not(unix))]
    {
        // Windows Limitation: Written without custom DACLs; relies on %LOCALAPPDATA% directory ACLs.
        std::fs::write(&tmp_path, content)?;
    }

    #[cfg(windows)]
    {
        if path.exists() {
            let _ = std::fs::remove_file(path);
        }
    }

    std::fs::rename(&tmp_path, path)?;
    Ok(())
}

/// Helper to safely read and deserialize the cache map from disk.
fn read_cache_map(path: &Path) -> HashMap<String, CachedRemoteListing> {
    if let Ok(meta) = std::fs::symlink_metadata(path) {
        if meta.file_type().is_symlink() {
            let _ = std::fs::remove_file(path);
            return HashMap::new();
        }
    }
    if let Ok(content) = std::fs::read_to_string(path) {
        if content.len() <= 50 * 1024 * 1024 {
            if let Ok(map) = serde_json::from_str(&content) {
                return map;
            }
        }
    }
    HashMap::new()
}

/// Helper to safely serialize and atomically write the cache map to disk.
fn write_cache_map(path: &Path, cache: &HashMap<String, CachedRemoteListing>) {
    if let Ok(json) = serde_json::to_string_pretty(cache) {
        let _ = write_cache_atomic(path, &json);
    }
}

/// Loads cached remote items for remote_url and auth_user if valid and not expired.
///
/// Security:
/// - Validates that hrefs and relative paths do not contain directory traversal sequences (`has_traversal_sequence`).
/// - Legitimate hidden files (e.g. `.gitignore`, `.env`) are preserved.
/// - Does not rely on rigid prefix matching, correctly supporting root-relative hrefs returned by WebDAV servers.
pub fn load_remote_cache(remote_url: &str, auth_user: Option<&str>) -> Option<Vec<WebdavItem>> {
    let path = get_cache_file_path()?;
    let cache = read_cache_map(&path);
    let key = cache_key(remote_url, auth_user);
    let entry = cache.get(&key)?;

    let ttl = get_cache_ttl_secs();
    if ttl > 0 {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .ok()?
            .as_secs();
        if now.saturating_sub(entry.timestamp) > ttl {
            return None;
        }
    }

    let mut valid_items = Vec::with_capacity(entry.items.len());
    for item in &entry.items {
        if has_traversal_sequence(&item.href) {
            continue;
        }
        let rel = relative_item_path(remote_url, &item.href);
        if has_traversal_sequence(&rel) {
            continue;
        }
        valid_items.push(item.clone());
    }

    Some(valid_items)
}

/// Saves remote items to the local cache file for remote_url and auth_user.
///
/// Filters out any item with traversal sequences (`has_traversal_sequence`) to prevent cache poisoning.
/// Cached listings are keyed by normalized collection URL and authenticated user.
pub fn save_remote_cache(remote_url: &str, auth_user: Option<&str>, items: &[WebdavItem]) {
    let path = match get_cache_file_path() {
        Some(p) => p,
        None => return,
    };
    let mut cache = read_cache_map(&path);

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);

    let mut safe_items = Vec::with_capacity(items.len());
    for item in items {
        if has_traversal_sequence(&item.href) {
            continue;
        }
        let rel = relative_item_path(remote_url, &item.href);
        if has_traversal_sequence(&rel) {
            continue;
        }
        safe_items.push(item.clone());
    }

    let key = cache_key(remote_url, auth_user);
    cache.insert(
        key,
        CachedRemoteListing {
            remote_url: remote_url.to_string(),
            auth_user: auth_user.map(|u| u.trim().to_ascii_lowercase()),
            timestamp: now,
            items: safe_items,
        },
    );

    write_cache_map(&path, &cache);
}

/// Updates or inserts an item in the remote cache after upload.
pub fn update_remote_cache_item(
    remote_url: &str,
    auth_user: Option<&str>,
    rel_path: &str,
    new_size: u64,
    new_mtime: Option<u64>,
) {
    if rel_path.is_empty() || has_traversal_sequence(rel_path) || rel_path.starts_with('/') {
        return;
    }
    let path = match get_cache_file_path() {
        Some(p) => p,
        None => return,
    };
    let mut cache = read_cache_map(&path);
    if cache.is_empty() {
        return;
    }

    let key = cache_key(remote_url, auth_user);
    if let Some(entry) = cache.get_mut(&key) {
        let expected_url = build_file_url(remote_url, rel_path);
        let mut found = false;
        for item in &mut entry.items {
            let item_rel = relative_item_path(remote_url, &item.href);
            if item_rel == rel_path || item.href.eq_ignore_ascii_case(&expected_url) {
                item.size = new_size;
                item.mtime = new_mtime;
                found = true;
                break;
            }
        }
        if !found {
            entry.items.push(WebdavItem {
                href: expected_url,
                is_dir: false,
                size: new_size,
                mtime: new_mtime,
            });
        }
        write_cache_map(&path, &cache);
    }
}

/// Removes an item from the remote cache after deletion.
pub fn remove_remote_cache_item(remote_url: &str, auth_user: Option<&str>, rel_path: &str) {
    if rel_path.is_empty() || has_traversal_sequence(rel_path) || rel_path.starts_with('/') {
        return;
    }
    let path = match get_cache_file_path() {
        Some(p) => p,
        None => return,
    };
    let mut cache = read_cache_map(&path);
    if cache.is_empty() {
        return;
    }

    let key = cache_key(remote_url, auth_user);
    if let Some(entry) = cache.get_mut(&key) {
        let expected_url = build_file_url(remote_url, rel_path);
        entry.items.retain(|item| {
            let item_rel = relative_item_path(remote_url, &item.href);
            item_rel != rel_path && !item.href.eq_ignore_ascii_case(&expected_url)
        });
        write_cache_map(&path, &cache);
    }
}

/// Clears the remote listing cache file.
pub fn clear_remote_cache() -> Result<(), String> {
    let path = match get_cache_file_path() {
        Some(p) => p,
        None => return Ok(()),
    };
    if let Ok(meta) = std::fs::symlink_metadata(&path) {
        if meta.file_type().is_symlink() {
            let _ = std::fs::remove_file(&path);
            return Ok(());
        }
    }
    if path.exists() {
        std::fs::remove_file(&path).map_err(|e| format!("Failed to clear cache: {}", e))?;
    }
    Ok(())
}

/// Parses a WebDAV date string into a UNIX timestamp (seconds since epoch).
pub fn parse_webdav_date(date_str: &str) -> Option<u64> {
    let s = date_str.trim();
    if s.is_empty() {
        return None;
    }
    // 1. Standard HTTP-date / RFC 2822 (e.g. "Mon, 28 Sep 2026 13:45:09 GMT")
    if let Ok(dt) = chrono::DateTime::parse_from_rfc2822(s) {
        return Some(dt.timestamp().max(0) as u64);
    }
    // 2. ISO 8601 / RFC 3339 (e.g. "2026-09-28T13:45:09Z")
    if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(s) {
        return Some(dt.timestamp().max(0) as u64);
    }
    // 3. RFC 850 format (e.g. "Monday, 28-Sep-26 13:45:09 GMT")
    if let Ok(dt) = chrono::NaiveDateTime::parse_from_str(s, "%A, %d-%b-%y %H:%M:%S GMT") {
        return Some(dt.and_utc().timestamp().max(0) as u64);
    }
    // 4. asctime format (e.g. "Mon Sep 28 13:45:09 2026")
    if let Ok(dt) = chrono::NaiveDateTime::parse_from_str(s, "%a %b %e %H:%M:%S %Y") {
        return Some(dt.and_utc().timestamp().max(0) as u64);
    }
    None
}

/// Parses the WebDAV PROPFIND XML response into WebdavItem records.
pub fn parse_propfind_xml(xml: &str) -> Vec<WebdavItem> {
    let mut items = Vec::new();
    let mut reader = quick_xml::Reader::from_str(xml);
    reader.config_mut().trim_text(true);

    let mut in_response = false;
    let mut in_href = false;
    let mut in_resourcetype = false;
    let mut in_getcontentlength = false;
    let mut in_iscollection = false;
    let mut in_getlastmodified = false;

    let mut current_href = String::new();
    let mut current_is_dir = false;
    let mut current_size: u64 = 0;
    let mut current_mtime: Option<u64> = None;

    let mut buf = Vec::new();

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(quick_xml::events::Event::Start(ref e)) => {
                let local = e.local_name();
                let name = local.as_ref();
                if name.eq_ignore_ascii_case(b"response") {
                    in_response = true;
                    current_href.clear();
                    current_is_dir = false;
                    current_size = 0;
                    current_mtime = None;
                } else if in_response && name.eq_ignore_ascii_case(b"href") {
                    in_href = true;
                } else if in_response && name.eq_ignore_ascii_case(b"resourcetype") {
                    in_resourcetype = true;
                } else if in_resourcetype && name.eq_ignore_ascii_case(b"collection") {
                    current_is_dir = true;
                } else if in_response && name.eq_ignore_ascii_case(b"getcontentlength") {
                    in_getcontentlength = true;
                } else if in_response && name.eq_ignore_ascii_case(b"getlastmodified") {
                    in_getlastmodified = true;
                } else if in_response
                    && (name.eq_ignore_ascii_case(b"iscollection")
                        || name.eq_ignore_ascii_case(b"isfolder"))
                {
                    in_iscollection = true;
                }
            }
            Ok(quick_xml::events::Event::Empty(ref e)) => {
                let local = e.local_name();
                let name = local.as_ref();
                if in_resourcetype && name.eq_ignore_ascii_case(b"collection") {
                    current_is_dir = true;
                }
            }
            Ok(quick_xml::events::Event::Text(ref e)) => {
                if in_href {
                    if let Ok(text) = e.unescape() {
                        current_href.push_str(&text);
                    }
                } else if in_getcontentlength {
                    if let Ok(text) = e.unescape() {
                        current_size = text.trim().parse::<u64>().unwrap_or(0);
                    }
                } else if in_getlastmodified {
                    if let Ok(text) = e.unescape() {
                        current_mtime = parse_webdav_date(&text);
                    }
                } else if in_iscollection {
                    if let Ok(text) = e.unescape() {
                        let val = text.trim();
                        if val == "1" || val.eq_ignore_ascii_case("true") {
                            current_is_dir = true;
                        }
                    }
                }
            }
            Ok(quick_xml::events::Event::CData(ref e)) => {
                if in_href {
                    if let Ok(text) = std::str::from_utf8(e.as_ref()) {
                        current_href.push_str(text);
                    }
                } else if in_getcontentlength {
                    if let Ok(text) = std::str::from_utf8(e.as_ref()) {
                        current_size = text.trim().parse::<u64>().unwrap_or(0);
                    }
                } else if in_getlastmodified {
                    if let Ok(text) = std::str::from_utf8(e.as_ref()) {
                        current_mtime = parse_webdav_date(text);
                    }
                } else if in_iscollection {
                    if let Ok(text) = std::str::from_utf8(e.as_ref()) {
                        let val = text.trim();
                        if val == "1" || val.eq_ignore_ascii_case("true") {
                            current_is_dir = true;
                        }
                    }
                }
            }
            Ok(quick_xml::events::Event::End(ref e)) => {
                let local = e.local_name();
                let name = local.as_ref();
                if name.eq_ignore_ascii_case(b"response") {
                    in_response = false;
                    let clean_href = current_href.trim();
                    if !clean_href.is_empty() {
                        let is_dir = current_is_dir || clean_href.ends_with('/');
                        items.push(WebdavItem {
                            href: clean_href.to_string(),
                            is_dir,
                            size: current_size,
                            mtime: current_mtime,
                        });
                    }
                } else if name.eq_ignore_ascii_case(b"href") {
                    in_href = false;
                } else if name.eq_ignore_ascii_case(b"resourcetype") {
                    in_resourcetype = false;
                } else if name.eq_ignore_ascii_case(b"getcontentlength") {
                    in_getcontentlength = false;
                } else if name.eq_ignore_ascii_case(b"getlastmodified") {
                    in_getlastmodified = false;
                } else if name.eq_ignore_ascii_case(b"iscollection")
                    || name.eq_ignore_ascii_case(b"isfolder")
                {
                    in_iscollection = false;
                }
            }
            Ok(quick_xml::events::Event::Eof) => break,
            Err(_) => break,
            _ => {}
        }
        buf.clear();
    }

    items
}

/// Simple percent decoder for URI path segments.
fn decode_percent(s: &str) -> String {
    let mut result = Vec::new();
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Ok(byte) =
                u8::from_str_radix(std::str::from_utf8(&bytes[i + 1..i + 3]).unwrap_or(""), 16)
            {
                result.push(byte);
                i += 3;
                continue;
            }
        }
        result.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&result).to_string()
}

/// Resolves an item's href against the base URL, handling absolute URLs,
/// absolute paths (/...), and relative paths.
pub fn resolve_item_url(base_url: &str, item_href: &str) -> String {
    let safe_href = if item_href.contains(' ') {
        item_href.replace(' ', "%20")
    } else {
        item_href.to_string()
    };
    if let Ok(base) = rustydav::prelude::Url::parse(base_url) {
        if let Ok(joined) = base.join(&safe_href) {
            let mut s = joined.to_string();
            if item_href.ends_with('/') && !s.ends_with('/') {
                s.push('/');
            }
            return s;
        }
    }
    let clean_base = base_url.trim_end_matches('/');
    let clean_href = safe_href.trim_start_matches('/');
    format!("{}/{}", clean_base, clean_href)
}

/// Checks whether a relative path segment is safe and free of directory traversal sequences.
pub fn is_safe_relative_path(path: &str) -> bool {
    let trimmed = path.trim();
    if trimmed.is_empty() || trimmed.contains('\\') || trimmed.starts_with('/') {
        return false;
    }
    let p = Path::new(trimmed);
    if p.is_absolute() {
        return false;
    }
    for part in trimmed.split('/') {
        let part_trimmed = part.trim();
        if part_trimmed.is_empty() || part_trimmed == "." || part_trimmed == ".." {
            return false;
        }
    }
    for comp in p.components() {
        match comp {
            std::path::Component::Normal(_) => {}
            _ => return false,
        }
    }
    true
}

/// Safely joins a relative path to a base directory, rejecting any path traversal attempts.
pub fn safe_join_path(base: &Path, rel: &str) -> Result<PathBuf, String> {
    if !is_safe_relative_path(rel) {
        return Err(format!("Unsafe relative path segment rejected: {}", rel));
    }
    let target = base.join(rel);
    if let Ok(c_base) = base.canonicalize() {
        let mut cur = target.as_path();
        while let Some(parent) = cur.parent() {
            if let Ok(c_parent) = parent.canonicalize() {
                if !c_parent.starts_with(&c_base) {
                    return Err(format!("Path traversal attempt detected: {}", rel));
                }
                break;
            }
            cur = parent;
        }
    }
    Ok(target)
}

fn sanitize_relative_path(rel: &str) -> String {
    let clean = rel.trim_start_matches('/');
    if is_safe_relative_path(clean) {
        clean.to_string()
    } else {
        String::new()
    }
}

/// Computes the item path relative to the collection URL.
pub fn relative_item_path(base_url: &str, item_href: &str) -> String {
    let resolved_url_str = resolve_item_url(base_url, item_href);
    let resolved_url = match rustydav::prelude::Url::parse(&resolved_url_str) {
        Ok(u) => u,
        Err(_) => return sanitize_relative_path(&decode_percent(item_href)),
    };
    let base_url_parsed = match rustydav::prelude::Url::parse(base_url) {
        Ok(u) => u,
        Err(_) => return sanitize_relative_path(&decode_percent(item_href)),
    };

    let base_path = decode_percent(base_url_parsed.path());
    let item_path = decode_percent(resolved_url.path());

    let trimmed_base = base_path.trim_end_matches('/');
    let trimmed_item = item_path.trim_end_matches('/');

    // 1. Exact match (case-insensitive) - collection root itself
    if trimmed_item.eq_ignore_ascii_case(trimmed_base) {
        return String::new();
    }

    // 2. Direct prefix match (case-insensitive)
    if trimmed_item.len() > trimmed_base.len() {
        let (prefix, suffix) = trimmed_item.split_at(trimmed_base.len());
        if prefix.eq_ignore_ascii_case(trimmed_base)
            && (suffix.starts_with('/') || trimmed_base.is_empty())
        {
            return sanitize_relative_path(suffix);
        }
    }

    // 3. Substring match for base path in item path
    if !trimmed_base.is_empty() {
        let lower_item = trimmed_item.to_ascii_lowercase();
        let lower_base = trimmed_base.to_ascii_lowercase();
        if let Some(idx) = lower_item.find(&lower_base) {
            let after = &trimmed_item[idx + trimmed_base.len()..];
            let rel = after.trim_start_matches('/');
            if !rel.is_empty() {
                return sanitize_relative_path(rel);
            }
        }
    }

    // 4. Substring match on last segment of base path (e.g. course code)
    let last_seg = trimmed_base.rsplit('/').next().unwrap_or("");
    if !last_seg.is_empty() {
        let lower_item = trimmed_item.to_ascii_lowercase();
        let lower_seg = format!("/{}", last_seg.to_ascii_lowercase());
        if let Some(idx) = lower_item.find(&lower_seg) {
            let after = &trimmed_item[idx + lower_seg.len()..];
            let rel = after.trim_start_matches('/');
            if !rel.is_empty() {
                return sanitize_relative_path(rel);
            }
        }
    }

    let raw_rel = trimmed_item.rsplit('/').next().unwrap_or("");
    sanitize_relative_path(raw_rel)
}

/// Builds the canonical remote collection URL with trailing slash.
pub fn build_remote_url(base_url: &str, subdir: &str) -> String {
    let clean_base = base_url.trim().trim_end_matches('/');
    let clean_sub = subdir.trim().trim_matches('/');
    if clean_sub.is_empty() {
        format!("{}/", clean_base)
    } else {
        format!("{}/{}/", clean_base, clean_sub)
    }
}

/// Builds the URL for a specific file under a collection, ensuring proper percent-encoding.
pub fn build_file_url(collection_url: &str, rel_path: &str) -> String {
    if let Ok(mut base) = rustydav::prelude::Url::parse(collection_url) {
        let ok = if let Ok(mut segments) = base.path_segments_mut() {
            segments.pop_if_empty();
            for part in rel_path.split('/') {
                let trimmed = part.trim();
                if !trimmed.is_empty() {
                    segments.push(trimmed);
                }
            }
            true
        } else {
            false
        };
        if ok {
            return base.to_string();
        }
    }
    let clean_col = collection_url.trim_end_matches('/');
    let clean_rel = rel_path.trim_start_matches('/');
    format!("{}/{}", clean_col, clean_rel)
}

/// Validates a WebDAV URL against SSRF, internal networks, and insecure protocols.
/// In production, requires HTTPS and blocks local/private/link-local addresses.
pub fn validate_webdav_url(url_str: &str) -> Result<(), String> {
    let allow_insecure = cfg!(test)
        || std::env::var("ALLOW_INSECURE_WEBDAV").as_deref() == Ok("1")
        || std::env::var("TEST_WEBDAV_URL").is_ok();
    validate_webdav_url_internal(url_str, allow_insecure)
}

pub fn validate_webdav_url_internal(url_str: &str, allow_insecure: bool) -> Result<(), String> {
    let url = rustydav::prelude::Url::parse(url_str)
        .map_err(|e| format!("Invalid WebDAV URL: {}", e))?;

    let scheme = url.scheme().to_ascii_lowercase();
    if scheme != "https" {
        if scheme == "http" && allow_insecure {
            // Permitted for tests or explicit development override
        } else {
            return Err("Insecure protocol: WebDAV requires HTTPS in production.".to_string());
        }
    }

    if allow_insecure {
        return Ok(());
    }

    let host = url.host_str().ok_or_else(|| "URL has no host.".to_string())?;
    let lower_host = host.to_ascii_lowercase();

    // Block localhost and internal domains
    if lower_host == "localhost"
        || lower_host.ends_with(".localhost")
        || lower_host.ends_with(".local")
        || lower_host.ends_with(".internal")
        || lower_host.ends_with(".lan")
    {
        return Err("Access to internal/loopback hostname is blocked for security.".to_string());
    }

    // Check IP addresses for private / loopback / link-local / metadata ranges
    if let Ok(ip) = host.parse::<std::net::IpAddr>() {
        match ip {
            std::net::IpAddr::V4(ipv4) => {
                if ipv4.is_loopback() {
                    return Err("Access to loopback IP is blocked for security.".to_string());
                }
                if ipv4.is_link_local() {
                    return Err("Access to link-local IP (cloud metadata) is blocked for security.".to_string());
                }
                if ipv4.is_private() {
                    return Err("Access to private RFC 1918 IP is blocked for security.".to_string());
                }
                if ipv4.is_broadcast() || ipv4.is_unspecified() {
                    return Err("Access to broadcast/unspecified IP is blocked for security.".to_string());
                }
                let octets = ipv4.octets();
                // 100.64.0.0/10 Carrier-grade NAT
                if octets[0] == 100 && (octets[1] & 0xC0) == 64 {
                    return Err("Access to shared carrier-grade NAT IP is blocked for security.".to_string());
                }
            }
            std::net::IpAddr::V6(ipv6) => {
                if ipv6.is_loopback() {
                    return Err("Access to loopback IPv6 is blocked for security.".to_string());
                }
                if ipv6.is_unspecified() {
                    return Err("Access to unspecified IPv6 is blocked for security.".to_string());
                }
                let segments = ipv6.segments();
                // fe80::/10 link-local
                if (segments[0] & 0xffc0) == 0xfe80 {
                    return Err("Access to link-local IPv6 is blocked for security.".to_string());
                }
                // fc00::/7 unique local address
                if (segments[0] & 0xfe00) == 0xfc00 {
                    return Err("Access to unique local IPv6 is blocked for security.".to_string());
                }
            }
        }
    }

    Ok(())
}

/// Ensures all parent collections exist for a relative file path prior to PUT.
pub fn ensure_remote_parent_dirs(
    client: &rustydav::client::Client,
    remote_url: &str,
    rel_path: &str,
) -> Result<(), String> {
    validate_webdav_url(remote_url)?;

    if !is_safe_relative_path(rel_path) {
        return Err(format!("Unsafe relative path rejected: {}", rel_path));
    }

    let res = client.mkcol(remote_url);
    if let Ok(r) = res {
        let status = r.status().as_u16();
        if status != 201 && status != 405 && status != 200 && status != 301 && status != 302 {
            if status == 401 || status == 403 {
                return Err(format!("MKCOL failed for {}: HTTP {}", remote_url, status));
            }
        }
    } else if let Err(e) = res {
        return Err(format!("MKCOL network error for {}: {}", remote_url, e));
    }

    let parts: Vec<&str> = rel_path.split('/').collect();
    if parts.len() <= 1 {
        return Ok(());
    }
    let mut current_rel = String::new();
    for part in &parts[..parts.len() - 1] {
        if !current_rel.is_empty() {
            current_rel.push('/');
        }
        current_rel.push_str(part);
        let dir_url = format!("{}/", build_file_url(remote_url, &current_rel));
        let res = client
            .mkcol(&dir_url)
            .map_err(|e| format!("Failed to create folder {}: {}", dir_url, e))?;
        let status = res.status().as_u16();
        if status != 201 && status != 405 && status != 200 && status != 301 && status != 302 {
            if status == 401 || status == 403 {
                return Err(format!("Failed to create remote directory {}: HTTP {}", dir_url, status));
            }
        }
    }
    Ok(())
}

/// Recursively lists remote WebDAV items under remote_url.
/// First attempts Depth: infinity. If Depth: infinity fails, returns HTTP error,
/// or returns 0 child items (e.g. server restricts Depth: infinity to Depth: 0),
/// it falls back to breadth-first traversal using Depth: 1.
pub fn list_remote_recursive(
    client: &rustydav::client::Client,
    remote_url: &str,
    cancel_flag: &AtomicBool,
) -> Result<Vec<WebdavItem>, String> {
    list_remote_recursive_with_log(client, remote_url, cancel_flag, |_| {})
}

/// Recursively lists remote WebDAV items under remote_url with streamed diagnostic logs.
pub fn list_remote_recursive_with_log<F>(
    client: &rustydav::client::Client,
    remote_url: &str,
    cancel_flag: &AtomicBool,
    log: F,
) -> Result<Vec<WebdavItem>, String>
where
    F: FnMut(&str),
{
    list_remote_recursive_with_concurrency_and_log(client, remote_url, cancel_flag, None, None, log)
}

/// Recursively lists remote WebDAV items under remote_url with configurable worker concurrency and streamed diagnostic logs.
///
/// Thread Safety:
/// `rustydav::client::Client` implements `Send + Sync` (internally backed by `reqwest::blocking::Client` connection pool)
/// allowing concurrent Depth: 1 requests across worker threads.
///
/// Parameters:
/// - `concurrency`: Worker thread count (1..64). If `None`, defaults to `get_scan_concurrency()`.
/// - `auth_user`: Optional username for credential-scoped cache partitioning (CWE-287 / CWE-384).
pub fn list_remote_recursive_with_concurrency_and_log<F>(
    client: &rustydav::client::Client,
    remote_url: &str,
    cancel_flag: &AtomicBool,
    concurrency: Option<usize>,
    auth_user: Option<&str>,
    log: F,
) -> Result<Vec<WebdavItem>, String>
where
    F: FnMut(&str),
{
    list_remote_recursive_with_options_and_log(
        client,
        remote_url,
        cancel_flag,
        concurrency,
        auth_user,
        true,
        log,
    )
}

/// Recursively lists remote WebDAV items under remote_url with configurable worker concurrency, cache control, and streamed diagnostic logs.
///
/// Parameters:
/// - `concurrency`: Worker thread count (1..64). If `None`, defaults to `get_scan_concurrency()`.
/// - `auth_user`: Optional username for credential-scoped cache partitioning (CWE-287 / CWE-384).
/// - `use_cache`: When true and cache is enabled, loads from/saves to remote listing cache. When false (e.g. GET), bypasses cache.
pub fn list_remote_recursive_with_options_and_log<F>(
    client: &rustydav::client::Client,
    remote_url: &str,
    cancel_flag: &AtomicBool,
    concurrency: Option<usize>,
    auth_user: Option<&str>,
    use_cache: bool,
    mut log: F,
) -> Result<Vec<WebdavItem>, String>
where
    F: FnMut(&str),
{
    validate_webdav_url(remote_url)?;
    if cancel_flag.load(Ordering::SeqCst) {
        return Err("Operation canceled by user.".to_string());
    }

    if use_cache && is_cache_enabled() {
        if let Some(cached_items) = load_remote_cache(remote_url, auth_user) {
            log(&format!(
                "Loaded {} item(s) from remote listing cache for {}.\n",
                cached_items.len(),
                remote_url
            ));
            return Ok(cached_items);
        }
    }

    // Try Depth: infinity first
    log(&format!(
        "Querying remote server with Depth: infinity for {}...\n",
        remote_url
    ));
    match client.list(remote_url, "infinity") {
        Ok(res) => {
            let status = res.status();
            if status.is_success() || status.as_u16() == 207 {
                let body = res.text().unwrap_or_default();
                let items = parse_propfind_xml(&body);
                // Depth: infinity is only accepted if it returned child items (not just the root collection itself)
                let child_count = items
                    .iter()
                    .filter(|i| !relative_item_path(remote_url, &i.href).is_empty())
                    .count();
                if child_count > 0 {
                    log(&format!(
                        "Server returned {} items via Depth: infinity.\n",
                        child_count
                    ));
                    if use_cache && is_cache_enabled() {
                        save_remote_cache(remote_url, auth_user, &items);
                    }
                    return Ok(items);
                } else {
                    log("Depth: infinity returned 0 child items (server likely restricts Depth: infinity). Falling back to Depth: 1 traversal...\n");
                }
            } else {
                log(&format!(
                    "Server responded with HTTP {} for Depth: infinity. Falling back to Depth: 1 traversal...\n",
                    status
                ));
            }
        }
        Err(e) => {
            log(&format!(
                "Depth: infinity request failed ({}). Falling back to Depth: 1 traversal...\n",
                e
            ));
        }
    }

    // Fallback: Concurrent Breadth-First-Search traversal using Depth: 1
    log("Scanning directories using Depth: 1...\n");

    let num_workers = concurrency
        .map(|c| c.clamp(1, MAX_SCAN_CONCURRENCY))
        .unwrap_or_else(get_scan_concurrency);
    let remote_parsed = rustydav::prelude::Url::parse(remote_url)
        .map_err(|e| format!("Invalid remote URL: {}", e))?;
    let remote_origin = remote_parsed.origin();

    let root_visited_key = remote_url.trim_end_matches('/').to_ascii_lowercase();
    let mut initial_visited = HashSet::new();
    initial_visited.insert(root_visited_key);

    let mut initial_queue = VecDeque::new();
    initial_queue.push_back(remote_url.to_string());

    struct ScanState {
        queue: VecDeque<String>,
        active_workers: usize,
        visited: HashSet<String>,
        seen_items: HashSet<(bool, String)>,
        all_items: Vec<WebdavItem>,
        scanned_count: usize,
        error: Option<String>,
        stopped: bool,
    }

    struct WorkerGuard<'a> {
        state_ref: &'a Mutex<ScanState>,
        cvar_ref: &'a Condvar,
    }

    impl<'a> WorkerGuard<'a> {
        fn acquire(
            state_ref: &'a Mutex<ScanState>,
            cvar_ref: &'a Condvar,
            cancel_flag: &AtomicBool,
        ) -> Option<(Self, String, usize)> {
            let mut state = match state_ref.lock() {
                Ok(s) => s,
                Err(poisoned) => {
                    eprintln!("[webdav scan] Mutex poisoned on lock; recovering state.");
                    let s = poisoned.into_inner();
                    if s.stopped || s.error.is_some() {
                        return None;
                    }
                    s
                }
            };
            loop {
                if cancel_flag.load(Ordering::SeqCst) {
                    state.stopped = true;
                    cvar_ref.notify_all();
                    return None;
                }
                if state.stopped || state.error.is_some() {
                    return None;
                }
                if let Some(url) = state.queue.pop_front() {
                    state.active_workers += 1;
                    state.scanned_count += 1;
                    let scan_idx = state.scanned_count;
                    let guard = Self {
                        state_ref,
                        cvar_ref,
                    };
                    return Some((guard, url, scan_idx));
                }
                if state.active_workers == 0 {
                    state.stopped = true;
                    cvar_ref.notify_all();
                    return None;
                }
                let res = cvar_ref.wait_timeout(state, Duration::from_millis(250));
                match res {
                    Ok((new_state, _)) => state = new_state,
                    Err(poisoned) => {
                        eprintln!("[webdav scan] Mutex poisoned during worker wait; recovering state.");
                        let (new_state, _) = poisoned.into_inner();
                        if new_state.stopped || new_state.error.is_some() {
                            return None;
                        }
                        state = new_state;
                    }
                }
                // Check cancellation and stopped state immediately after reacquiring lock from wait
                if cancel_flag.load(Ordering::SeqCst) {
                    state.stopped = true;
                    cvar_ref.notify_all();
                    return None;
                }
                if state.stopped || state.error.is_some() {
                    return None;
                }
            }
        }
    }

    impl<'a> Drop for WorkerGuard<'a> {
        fn drop(&mut self) {
            let mut state = self.state_ref.lock().unwrap_or_else(|p| {
                eprintln!("[webdav scan] Mutex poisoned during worker drop; recovering state.");
                p.into_inner()
            });
            state.active_workers = state.active_workers.saturating_sub(1);
            if std::thread::panicking() {
                state.error = Some("WebDAV worker thread panicked unexpectedly.".to_string());
                state.stopped = true;
            }
            self.cvar_ref.notify_all();
        }
    }

    let state_mutex = Mutex::new(ScanState {
        queue: initial_queue,
        active_workers: 0,
        visited: initial_visited,
        seen_items: HashSet::new(),
        all_items: Vec::new(),
        scanned_count: 0,
        error: None,
        stopped: false,
    });
    let cvar = Condvar::new();

    let (log_tx, log_rx) = mpsc::channel::<String>();

    std::thread::scope(|s| {
        for _ in 0..num_workers {
            let worker_log_tx = log_tx.clone();
            let worker_remote_origin = remote_origin.clone();
            let state_ref = &state_mutex;
            let cvar_ref = &cvar;

            s.spawn(move || {
                let run_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    loop {
                        let (guard, current_url, scan_idx) = match WorkerGuard::acquire(
                            state_ref,
                            cvar_ref,
                            cancel_flag,
                        ) {
                            Some(triple) => triple,
                            None => return,
                        };

                        let rel_folder = relative_item_path(remote_url, &current_url);
                        let display_folder = if rel_folder.is_empty() {
                            "/ (root)".to_string()
                        } else {
                            rel_folder
                        };

                        let _ = worker_log_tx.send(format!(
                            "[{}] Scanning directory: {}...\n",
                            scan_idx, display_folder
                        ));

                        // Check cancellation before issuing directory listing network request
                        if cancel_flag.load(Ordering::SeqCst) {
                            let mut state = state_ref.lock().unwrap_or_else(|p| p.into_inner());
                            state.stopped = true;
                            return;
                        }

                        let list_res = client.list(&current_url, "1");

                        // Check cancellation immediately after blocking network I/O returns to abort before parsing response body
                        if cancel_flag.load(Ordering::SeqCst) {
                            let mut state = state_ref.lock().unwrap_or_else(|p| p.into_inner());
                            state.stopped = true;
                            return;
                        }

                        let res = match list_res {
                            Ok(r) => r,
                            Err(e) => {
                                let mut state = state_ref.lock().unwrap_or_else(|p| p.into_inner());
                                state.error = Some(format!("List request failed for {}: {}", current_url, e));
                                state.stopped = true;
                                return;
                            }
                        };

                        let status = res.status();
                        if !status.is_success() && status.as_u16() != 207 {
                            let mut state = state_ref.lock().unwrap_or_else(|p| p.into_inner());
                            if scan_idx == 1 && status.as_u16() == 404 {
                                let _ = worker_log_tx.send(
                                    "Remote directory does not exist yet (404); starting with empty listing.\n"
                                        .to_string(),
                                );
                                state.stopped = true;
                                return;
                            }
                            state.error = Some(format!("Server returned HTTP {} for {}", status, current_url));
                            state.stopped = true;
                            return;
                        }

                        let body = res.text().unwrap_or_default();
                        let items = parse_propfind_xml(&body);

                        let mut children_in_dir = 0;
                        let mut new_dirs = 0;
                        let mut new_files = 0;

                        let mut discovered_sub_urls = Vec::new();
                        let mut discovered_items = Vec::new();

                        for item in items {
                            let rel = relative_item_path(&current_url, &item.href);
                            if rel.is_empty() {
                                // Skips current collection directory itself
                                continue;
                            }
                            if !is_safe_relative_path(&rel) {
                                continue;
                            }

                            children_in_dir += 1;
                            if item.is_dir {
                                new_dirs += 1;
                                let sub_url = resolve_item_url(&current_url, &item.href);
                                if let Ok(parsed_sub) = rustydav::prelude::Url::parse(&sub_url) {
                                    if parsed_sub.origin() == worker_remote_origin {
                                        let rel_from_root = relative_item_path(remote_url, &sub_url);
                                        if !rel_from_root.is_empty() && is_safe_relative_path(&rel_from_root) {
                                            if validate_webdav_url(&sub_url).is_ok() {
                                                discovered_sub_urls.push(sub_url);
                                            }
                                        }
                                    }
                                }
                                discovered_items.push(item);
                            } else {
                                new_files += 1;
                                discovered_items.push(item);
                            }
                        }

                        if children_in_dir == 0 && current_url == remote_url {
                            let snippet_len = body.len().min(400);
                            let _ = worker_log_tx.send(format!(
                                "Notice: 0 items parsed from collection listing. Response preview:\n{}\n",
                                &body[..snippet_len]
                            ));
                        } else {
                            let _ = worker_log_tx.send(format!(
                                "   -> Found {} file(s) and {} subfolder(s) in {}\n",
                                new_files, new_dirs, display_folder
                            ));
                        }

                        {
                            let mut state = state_ref.lock().unwrap_or_else(|p| p.into_inner());
                            if state.stopped || state.error.is_some() {
                                return;
                            }

                            for item in discovered_items {
                                let key = (item.is_dir, item.href.clone());
                                if state.seen_items.insert(key) {
                                    state.all_items.push(item);
                                }
                            }

                            for sub_url in discovered_sub_urls {
                                let sub_key = sub_url.trim_end_matches('/').to_ascii_lowercase();
                                if !state.visited.contains(&sub_key) {
                                    if state.visited.len() >= MAX_SCANNED_DIRS_LIMIT {
                                        state.error = Some(format!(
                                            "Directory traversal limit reached ({} folders). Aborting scan for security.",
                                            MAX_SCANNED_DIRS_LIMIT
                                        ));
                                        state.stopped = true;
                                        return;
                                    }
                                    state.visited.insert(sub_key);
                                    state.queue.push_back(sub_url);
                                }
                            }
                        }
                        drop(guard);
                    }
                }));

                if run_result.is_err() {
                    let mut state = state_ref.lock().unwrap_or_else(|p| p.into_inner());
                    if state.error.is_none() {
                        state.error = Some("WebDAV worker thread panicked unexpectedly.".to_string());
                    }
                    state.stopped = true;
                    cvar_ref.notify_all();
                }
            });
        }

        drop(log_tx);

        while let Ok(msg) = log_rx.recv() {
            log(&msg);
        }
    });

    let mut state = state_mutex.into_inner().unwrap_or_else(|p| p.into_inner());

    if cancel_flag.load(Ordering::SeqCst) {
        return Err("Operation canceled by user.".to_string());
    }

    if let Some(err) = state.error {
        return Err(err);
    }

    state.all_items.sort_by(|a, b| a.href.cmp(&b.href));

    if use_cache && is_cache_enabled() {
        save_remote_cache(remote_url, auth_user, &state.all_items);
    }

    let file_count = state.all_items.iter().filter(|i| !i.is_dir).count();
    let dir_count = state.all_items.iter().filter(|i| i.is_dir).count();
    log(&format!(
        "\nScan complete: {} file(s) and {} subdirector(ies) discovered across {} folder(s).\n\n",
        file_count, dir_count, state.scanned_count
    ));

    Ok(state.all_items)

}

/// Extracts modification time from std::fs::Metadata as UNIX timestamp (seconds).
pub fn get_metadata_mtime(metadata: &std::fs::Metadata) -> Option<u64> {
    metadata
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
}

/// Walks a local directory recursively and returns (path, rel_path, size, mtime).
/// Skips symlinks to prevent directory traversal and arbitrary file disclosure.
pub fn collect_local_files_with_mtime(dir: &Path) -> Vec<(PathBuf, String, u64, Option<u64>)> {
    let mut files = Vec::new();
    if !dir.exists() || !dir.is_dir() {
        return files;
    }
    let canonical_base = match dir.canonicalize() {
        Ok(c) => c,
        Err(_) => return files,
    };
    let mut stack = vec![dir.to_path_buf()];
    while let Some(current_dir) = stack.pop() {
        if let Ok(entries) = std::fs::read_dir(&current_dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                let symlink_meta = match entry.file_type() {
                    Ok(ft) => {
                        // Skip symlinks completely to prevent arbitrary file read and directory escaping
                        if ft.is_symlink() {
                            continue;
                        }
                        ft
                    }
                    Err(_) => continue,
                };
                if symlink_meta.is_dir() {
                    stack.push(path);
                } else if symlink_meta.is_file() {
                    if let Ok(canonical_path) = path.canonicalize() {
                        if !canonical_path.starts_with(&canonical_base) {
                            continue;
                        }
                    }
                    if let Ok(rel) = path.strip_prefix(dir) {
                        let rel_str = rel.to_string_lossy().replace('\\', "/");
                        let meta = entry.metadata().ok();
                        let size = meta.as_ref().map(|m| m.len()).unwrap_or(0);
                        let mtime = meta.as_ref().and_then(get_metadata_mtime);
                        files.push((path, rel_str, size, mtime));
                    }
                }
            }
        }
    }
    files.sort_by(|a, b| a.1.cmp(&b.1));
    files
}

/// Walks a local directory recursively and returns (path, rel_path, size).
pub fn collect_local_files(dir: &Path) -> Vec<(PathBuf, String, u64)> {
    collect_local_files_with_mtime(dir)
        .into_iter()
        .map(|(p, r, s, _)| (p, r, s))
        .collect()
}

/// Determines whether a local file needs to be uploaded based on remote existence, file size, and timestamps.
///
/// Returns true if:
/// - File does not exist on remote.
/// - File sizes differ.
/// - File sizes match, but local file was modified after the remote file (with a 1-second margin for rounding).
pub fn should_upload_file(
    local_size: u64,
    local_mtime: Option<u64>,
    remote_size: u64,
    remote_mtime: Option<u64>,
) -> bool {
    if local_size != remote_size {
        return true;
    }
    match (local_mtime, remote_mtime) {
        (Some(l_time), Some(r_time)) => {
            // Local file is considered newer if its mtime is strictly greater than
            // remote mtime + 1s (to avoid false positives due to HTTP-date 1-second rounding).
            l_time > r_time + 1
        }
        // If timestamps are not both available, but sizes match, consider it up-to-date
        _ => false,
    }
}

/// Determines whether a remote file needs to be downloaded based on local existence, file size, and timestamps.
///
/// Returns true if:
/// - File does not exist locally.
/// - File sizes differ.
/// - File sizes match, but remote file was modified after the local file (with a 1-second margin for rounding/skew).
///
/// The 1-second margin accommodates HTTP-date 1-second resolution per RFC 7231 and minor client/server clock skew.
pub fn should_download_file(
    remote_size: u64,
    remote_mtime: Option<u64>,
    local_size: u64,
    local_mtime: Option<u64>,
) -> bool {
    if remote_size != local_size {
        return true;
    }
    match (remote_mtime, local_mtime) {
        (Some(r_time), Some(l_time)) => {
            // Remote file is considered newer if its mtime is strictly greater than
            // local mtime + 1s (to avoid false positives due to HTTP-date 1-second rounding or clock skew).
            r_time > l_time + 1
        }
        // If timestamps are not both available, but sizes match, consider it up-to-date
        _ => false,
    }
}

/// Computes SHA256 checksum formatted as hex string.
pub fn compute_sha256(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    format!("{:x}", hasher.finalize())
}

/// Verifies authentication by sending PROPFIND depth 0 to the URL.
pub fn verify_webdav_auth(url: &str, username: &str, password: &str) -> Result<(), String> {
    validate_webdav_url(url)?;
    let client = rustydav::client::Client::init(username, password);
    match client.list(url, "0") {
        Ok(res) => {
            let status = res.status();
            if status.is_success() || status.as_u16() == 207 {
                Ok(())
            } else if status.as_u16() == 401 || status.as_u16() == 403 {
                Err("Authentication failed: invalid username or password.".to_string())
            } else {
                Err(format!(
                    "WebDAV server returned status {}: {}",
                    status.as_u16(),
                    status.canonical_reason().unwrap_or("Unknown")
                ))
            }
        }
        Err(e) => Err(format!("Failed to connect to WebDAV server: {}", e)),
    }
}

/// Executes a native WebDAV action and streams logs through the callback.
pub fn execute_webdav_action<F>(
    client: &rustydav::client::Client,
    action: &str,
    remote_url: &str,
    local_dir: &Path,
    cancel_flag: &AtomicBool,
    log: F,
) -> Result<(), String>
where
    F: FnMut(&str),
{
    execute_webdav_action_with_options(client, action, remote_url, local_dir, cancel_flag, None, None, log)
}

/// Executes a native WebDAV action with custom options (such as scan concurrency and auth context) and streams logs.
pub fn execute_webdav_action_with_options<F>(
    client: &rustydav::client::Client,
    action: &str,
    remote_url: &str,
    local_dir: &Path,
    cancel_flag: &AtomicBool,
    concurrency: Option<usize>,
    auth_user: Option<&str>,
    mut log: F,
) -> Result<(), String>
where
    F: FnMut(&str),
{
    validate_webdav_url(remote_url)?;
    match action {
        "ls" => {
            log(&format!("Listing remote files in {}...\n", remote_url));
            let items = list_remote_recursive_with_concurrency_and_log(
                client, remote_url, cancel_flag, concurrency, auth_user, &mut log,
            )?;
            let mut count = 0;
            let mut total_size = 0;
            for item in &items {
                if item.is_dir {
                    continue;
                }
                let rel = relative_item_path(remote_url, &item.href);
                if rel.is_empty() {
                    continue;
                }
                log(&format!("{:>9} {}\n", item.size, rel));
                count += 1;
                total_size += item.size;
            }
            log(&format!(
                "\nTotal objects: {}, Total size: {} bytes\n",
                count, total_size
            ));
            Ok(())
        }
        "lsd" => {
            log(&format!("Listing remote directories in {}...\n", remote_url));
            let res = client
                .list(remote_url, "1")
                .map_err(|e| format!("List request failed: {}", e))?;
            if !res.status().is_success() && res.status().as_u16() != 207 {
                return Err(format!("Server returned HTTP {}", res.status()));
            }
            let body = res.text().unwrap_or_default();
            let items = parse_propfind_xml(&body);
            let mut count = 0;
            for item in &items {
                if !item.is_dir {
                    continue;
                }
                let rel = relative_item_path(remote_url, &item.href);
                if rel.is_empty() {
                    continue;
                }
                log(&format!("{:>9} {}/\n", -1, rel));
                count += 1;
            }
            log(&format!("\nTotal directories: {}\n", count));
            Ok(())
        }
        "put" | "put-dry" | "put-checksum" => {
            let is_dry = action == "put-dry";
            let with_checksum = action == "put-checksum";
            log(&format!("Reading local files in {}...\n", local_dir.display()));
            let local_files = collect_local_files_with_mtime(local_dir);
            if local_files.is_empty() {
                log("No local files found to copy.\n");
                return Ok(());
            }

            log(&format!(
                "Found {} local file(s). Querying remote files in {} to detect changes...\n",
                local_files.len(),
                remote_url
            ));
            let remote_items = list_remote_recursive_with_concurrency_and_log(
                client, remote_url, cancel_flag, concurrency, auth_user, &mut log,
            )?;
            let mut remote_map: HashMap<String, (u64, Option<u64>)> = HashMap::new();
            for item in remote_items {
                if item.is_dir {
                    continue;
                }
                let rel = relative_item_path(remote_url, &item.href);
                if !rel.is_empty() {
                    remote_map.insert(rel, (item.size, item.mtime));
                }
            }

            let mut files_to_upload = Vec::new();
            let mut skipped_count = 0;

            for (path, rel_str, local_size, local_mtime) in local_files {
                let needs_upload = match remote_map.get(&rel_str) {
                    None => true,
                    Some((rem_size, rem_mtime)) => {
                        should_upload_file(local_size, local_mtime, *rem_size, *rem_mtime)
                    }
                };

                if needs_upload {
                    files_to_upload.push((path, rel_str, local_size));
                } else {
                    skipped_count += 1;
                    if is_dry {
                        log(&format!(
                            "NOTICE: {}: Up to date (matches remote {} bytes), skipping\n",
                            rel_str, local_size
                        ));
                    }
                }
            }

            let total_to_upload = files_to_upload.len();
            log(&format!(
                "Scan complete: {} file(s) up to date, {} file(s) to upload.\n\n",
                skipped_count, total_to_upload
            ));

            if total_to_upload == 0 {
                log("All files are already up to date on remote.\n\nPut operation finished: 0 file(s) copied, all files up to date.\n");
                return Ok(());
            }

            let mut copied_count = 0;
            let max_size = get_max_file_size();
            for (idx, (path, rel_str, size)) in files_to_upload.into_iter().enumerate() {
                if cancel_flag.load(Ordering::SeqCst) {
                    log("Operation canceled by user.\n");
                    return Ok(());
                }
                let current_num = idx + 1;
                if size > max_size {
                    log(&format!(
                        "[{}/{}] ERROR: File {} exceeds maximum size limit ({} bytes > {} bytes), skipping.\n",
                        current_num, total_to_upload, rel_str, size, max_size
                    ));
                    continue;
                }
                let file_url = build_file_url(remote_url, &rel_str);
                if is_dry {
                    log(&format!(
                        "[{}/{}] NOTICE: {}: Would copy (new or modified, {} bytes)\n",
                        current_num, total_to_upload, rel_str, size
                    ));
                    continue;
                }
                log(&format!(
                    "[{}/{}] Uploading: {} ({} bytes)...\n",
                    current_num, total_to_upload, rel_str, size
                ));
                if let Err(e) = ensure_remote_parent_dirs(client, remote_url, &rel_str) {
                    log(&format!(
                        "[{}/{}] ERROR: Failed creating remote directory for {}: {}\n",
                        current_num, total_to_upload, rel_str, e
                    ));
                    continue;
                }
                let file = match std::fs::File::open(&path) {
                    Ok(f) => f,
                    Err(e) => {
                        log(&format!(
                            "[{}/{}] ERROR: Failed to open {}: {}\n",
                            current_num, total_to_upload, path.display(), e
                        ));
                        continue;
                    }
                };
                let checksum_str = if with_checksum {
                    match compute_file_sha256(&path) {
                        Ok(hash) => format!(" (sha256: {})", hash),
                        Err(_) => String::new(),
                    }
                } else {
                    String::new()
                };
                let res = client
                    .put(file, &file_url)
                    .map_err(|e| format!("Upload failed for {}: {}", rel_str, e))?;
                if res.status().is_success() {
                    copied_count += 1;
                    if is_cache_enabled() {
                        let mtime = std::fs::metadata(&path)
                            .ok()
                            .and_then(|m| get_metadata_mtime(&m));
                        update_remote_cache_item(remote_url, auth_user, &rel_str, size, mtime);
                    }
                    log(&format!(
                        "[{}/{}] Copied: {} ({} bytes){}\n",
                        current_num, total_to_upload, rel_str, size, checksum_str
                    ));
                } else {
                    log(&format!(
                        "[{}/{}] ERROR: Failed to copy {}: HTTP {}\n",
                        current_num, total_to_upload, rel_str, res.status()
                    ));
                }
            }
            log(&format!(
                "\nPut operation finished: {} file(s) copied, {} file(s) skipped (already up to date).\n",
                copied_count, skipped_count
            ));
            Ok(())
        }
        "get" | "get-dry" | "get-checksum" => {
            let is_dry = action == "get-dry";
            let with_checksum = action == "get-checksum";
            log(&format!("Querying remote files in {} with parallel scan...\n", remote_url));
            // Caching does not apply for GET; bypass cache to ensure fresh remote scan
            let items = list_remote_recursive_with_options_and_log(
                client, remote_url, cancel_flag, concurrency, auth_user, false, &mut log,
            )?;
            let remote_files: Vec<(&WebdavItem, String)> = items
                .iter()
                .filter(|item| !item.is_dir)
                .filter_map(|item| {
                    let rel = relative_item_path(remote_url, &item.href);
                    if rel.is_empty() || !is_safe_relative_path(&rel) {
                        None
                    } else {
                        Some((item, rel))
                    }
                })
                .collect();

            log(&format!("Scanning local files in {}...\n", local_dir.display()));
            let local_files = collect_local_files_with_mtime(local_dir);
            let mut local_map: HashMap<String, (u64, Option<u64>)> = HashMap::new();
            for (_, rel, size, mtime) in local_files {
                local_map.insert(rel, (size, mtime));
            }

            let mut files_to_download: Vec<(&WebdavItem, String)> = Vec::new();
            let mut skipped_count = 0;

            for (item, rel) in remote_files {
                let needs_download = match local_map.get(&rel) {
                    None => true,
                    Some((loc_size, loc_mtime)) => {
                        should_download_file(item.size, item.mtime, *loc_size, *loc_mtime)
                    }
                };

                if needs_download {
                    files_to_download.push((item, rel));
                } else {
                    skipped_count += 1;
                    if is_dry {
                        log(&format!(
                            "NOTICE: {}: Up to date (matches local {} bytes), skipping\n",
                            rel, item.size
                        ));
                    }
                }
            }

            let total_files = files_to_download.len();
            log(&format!(
                "Scan complete: {} file(s) up to date, {} file(s) to download.\n\n",
                skipped_count, total_files
            ));

            if total_files == 0 {
                log(&format!(
                    "All files are already up to date on local.\n\nGet operation finished: 0 file(s) downloaded, {} file(s) skipped (already up to date).\n",
                    skipped_count
                ));
                return Ok(());
            }

            let max_size = get_max_file_size();
            let mut count = 0;
            let mut total_bytes = 0;
            for (idx, (item, rel)) in files_to_download.iter().enumerate() {
                let current_num = idx + 1;
                if cancel_flag.load(Ordering::SeqCst) {
                    log("Operation canceled by user.\n");
                    return Ok(());
                }
                if item.size > max_size {
                    log(&format!(
                        "[{}/{}] ERROR: Remote file {} exceeds maximum size limit ({} bytes > {} bytes), skipping.\n",
                        current_num, total_files, rel, item.size, max_size
                    ));
                    continue;
                }
                if is_dry {
                    log(&format!(
                        "[{}/{}] NOTICE: {}: Would download (new or modified, {} bytes)\n",
                        current_num, total_files, rel, item.size
                    ));
                    continue;
                }
                log(&format!(
                    "[{}/{}] Downloading: {} ({} bytes)...\n",
                    current_num, total_files, rel, item.size
                ));
                let target_file = match safe_join_path(local_dir, rel) {
                    Ok(p) => p,
                    Err(e) => {
                        log(&format!(
                            "[{}/{}] ERROR: Unsafe relative path rejected for {}: {}\n",
                            current_num, total_files, rel, e
                        ));
                        continue;
                    }
                };
                if let Some(parent) = target_file.parent() {
                    std::fs::create_dir_all(parent).map_err(|e| {
                        format!("Failed to create directory {}: {}", parent.display(), e)
                    })?;
                    if let (Ok(c_parent), Ok(c_base)) = (parent.canonicalize(), local_dir.canonicalize()) {
                        if !c_parent.starts_with(&c_base) {
                            return Err(format!("Directory traversal detected for {}", rel));
                        }
                    }
                }
                let download_url = resolve_item_url(remote_url, &item.href);
                let get_res = client
                    .get(&download_url)
                    .map_err(|e| format!("Download failed for {}: {}", rel, e))?;
                if !get_res.status().is_success() {
                    log(&format!(
                        "[{}/{}] ERROR: Failed to download {}: HTTP {}\n",
                        current_num,
                        total_files,
                        rel,
                        get_res.status()
                    ));
                    continue;
                }

                let mut dest_file = std::fs::File::create(&target_file)
                    .map_err(|e| format!("Failed to create {}: {}", target_file.display(), e))?;
                let mut limited_reader = std::io::Read::take(get_res, max_size + 1);
                let downloaded_len = std::io::copy(&mut limited_reader, &mut dest_file)
                    .map_err(|e| format!("Failed to write {}: {}", target_file.display(), e))?;

                if downloaded_len > max_size {
                    drop(dest_file);
                    let _ = std::fs::remove_file(&target_file);
                    log(&format!(
                        "[{}/{}] ERROR: Downloaded file {} exceeded maximum limit ({} bytes), deleted.\n",
                        current_num, total_files, rel, max_size
                    ));
                    continue;
                }

                if let Some(mtime_sec) = item.mtime {
                    match std::fs::File::options().write(true).open(&target_file) {
                        Ok(file) => {
                            let sys_time = std::time::UNIX_EPOCH + std::time::Duration::from_secs(mtime_sec);
                            let times = std::fs::FileTimes::new().set_modified(sys_time);
                            if let Err(e) = file.set_times(times) {
                                log(&format!(
                                    "[{}/{}] WARNING: Could not preserve modification time on {}: {}\n",
                                    current_num, total_files, rel, e
                                ));
                            }
                        }
                        Err(e) => {
                            log(&format!(
                                "[{}/{}] WARNING: Could not open {} to set modification time: {}\n",
                                current_num, total_files, rel, e
                            ));
                        }
                    }
                }

                let checksum_str = if with_checksum {
                    match compute_file_sha256(&target_file) {
                        Ok(hash) => format!(" (sha256: {})", hash),
                        Err(_) => String::new(),
                    }
                } else {
                    String::new()
                };
                count += 1;
                total_bytes += downloaded_len as usize;
                log(&format!(
                    "[{}/{}] Downloaded: {} ({} bytes){}\n",
                    current_num,
                    total_files,
                    rel,
                    downloaded_len,
                    checksum_str
                ));
            }
            log(&format!(
                "\nGet operation finished: {} file(s) downloaded ({} bytes), {} file(s) skipped (already up to date).\n",
                count, total_bytes, skipped_count
            ));
            Ok(())
        }
        "check" => {
            log(&format!("Comparing local files with remote in {}...\n", remote_url));
            let remote_items = list_remote_recursive_with_concurrency_and_log(
                client, remote_url, cancel_flag, concurrency, auth_user, &mut log,
            )?;
            let mut remote_map: HashMap<String, u64> = HashMap::new();
            for item in remote_items {
                if item.is_dir {
                    continue;
                }
                let rel = relative_item_path(remote_url, &item.href);
                if !rel.is_empty() {
                    remote_map.insert(rel, item.size);
                }
            }
            let local_files = collect_local_files(local_dir);
            let mut matching = 0;
            let mut differences = 0;
            for (_, rel, local_size) in &local_files {
                match remote_map.remove(rel) {
                    Some(rem_size) => {
                        if *local_size == rem_size {
                            matching += 1;
                        } else {
                            differences += 1;
                            log(&format!(
                                "Difference in size: {} (local: {} bytes, remote: {} bytes)\n",
                                rel, local_size, rem_size
                            ));
                        }
                    }
                    None => {
                        differences += 1;
                        log(&format!("Missing in remote: {}\n", rel));
                    }
                }
            }
            for (rem_rel, _) in remote_map {
                differences += 1;
                log(&format!("Missing in local: {}\n", rem_rel));
            }
            log(&format!(
                "\nCheck summary: {} matching, {} differences.\n",
                matching, differences
            ));
            Ok(())
        }
        "sync" => {
            log(&format!(
                "Starting sync from {} to {}...\n",
                local_dir.display(),
                remote_url
            ));
            let local_files = collect_local_files_with_mtime(local_dir);
            let mut local_set: HashSet<String> = HashSet::new();

            log(&format!("Querying remote files in {} to detect changes...\n", remote_url));
            let remote_items = list_remote_recursive_with_concurrency_and_log(
                client, remote_url, cancel_flag, concurrency, auth_user, &mut log,
            )?;
            let mut remote_map: HashMap<String, (u64, Option<u64>)> = HashMap::new();
            for item in &remote_items {
                if item.is_dir {
                    continue;
                }
                let rel = relative_item_path(remote_url, &item.href);
                if !rel.is_empty() {
                    remote_map.insert(rel, (item.size, item.mtime));
                }
            }

            let mut files_to_upload = Vec::new();
            let mut skipped_count = 0;
            for (path, rel_str, local_size, local_mtime) in local_files {
                local_set.insert(rel_str.clone());
                let needs_upload = match remote_map.get(&rel_str) {
                    None => true,
                    Some((rem_size, rem_mtime)) => {
                        should_upload_file(local_size, local_mtime, *rem_size, *rem_mtime)
                    }
                };
                if needs_upload {
                    files_to_upload.push((path, rel_str, local_size));
                } else {
                    skipped_count += 1;
                }
            }

            let total_to_upload = files_to_upload.len();
            let mut uploaded = 0;
            let max_size = get_max_file_size();
            for (idx, (path, rel, size)) in files_to_upload.into_iter().enumerate() {
                if cancel_flag.load(Ordering::SeqCst) {
                    log("Operation canceled by user.\n");
                    return Ok(());
                }
                let current_num = idx + 1;
                if size > max_size {
                    log(&format!(
                        "[{}/{}] ERROR: File {} exceeds maximum size limit ({} bytes > {} bytes), skipping.\n",
                        current_num, total_to_upload, rel, size, max_size
                    ));
                    continue;
                }
                let file_url = build_file_url(remote_url, &rel);
                if let Err(e) = ensure_remote_parent_dirs(client, remote_url, &rel) {
                    log(&format!(
                        "[{}/{}] ERROR: Failed creating remote directory for {}: {}\n",
                        current_num, total_to_upload, rel, e
                    ));
                    continue;
                }
                let file = match std::fs::File::open(&path) {
                    Ok(f) => f,
                    Err(e) => {
                        log(&format!(
                            "[{}/{}] ERROR: Failed to open {}: {}\n",
                            current_num, total_to_upload, path.display(), e
                        ));
                        continue;
                    }
                };
                let res = client
                    .put(file, &file_url)
                    .map_err(|e| format!("Upload error: {}", e))?;
                if res.status().is_success() {
                    if is_cache_enabled() {
                        let mtime = std::fs::metadata(&path)
                            .ok()
                            .and_then(|m| get_metadata_mtime(&m));
                        update_remote_cache_item(remote_url, auth_user, &rel, size, mtime);
                    }
                    log(&format!("[{}/{}] Synced: {} ({} bytes)\n", current_num, total_to_upload, rel, size));
                    uploaded += 1;
                }
            }
            let mut deleted = 0;
            for item in remote_items {
                if item.is_dir {
                    continue;
                }
                let rel = relative_item_path(remote_url, &item.href);
                if !rel.is_empty() && !local_set.contains(&rel) {
                    if cancel_flag.load(Ordering::SeqCst) {
                        log("Operation canceled by user.\n");
                        return Ok(());
                    }
                    let file_url = resolve_item_url(remote_url, &item.href);
                    let del_res = client.delete(&file_url);
                    if del_res.is_ok() {
                        if is_cache_enabled() {
                            remove_remote_cache_item(remote_url, auth_user, &rel);
                        }
                        log(&format!("Deleted remote file not in local: {}\n", rel));
                        deleted += 1;
                    }
                }
            }
            log(&format!(
                "\nSync complete: {} synced, {} skipped (already up to date), {} remote files removed.\n",
                uploaded, skipped_count, deleted
            ));
            Ok(())
        }
        other => Err(format!("Unsupported WebDAV action: {}", other)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_propfind() {
        let sample = r#"<?xml version="1.0" encoding="utf-8"?>
        <D:multistatus xmlns:D="DAV:">
            <D:response>
                <D:href>/files/myuser/docs/</D:href>
                <D:propstat>
                    <D:prop>
                        <D:resourcetype><D:collection/></D:resourcetype>
                    </D:prop>
                    <D:status>HTTP/1.1 200 OK</D:status>
                </D:propstat>
            </D:response>
            <D:response>
                <D:href>/files/myuser/docs/notes.txt</D:href>
                <D:propstat>
                    <D:prop>
                        <D:getcontentlength>1024</D:getcontentlength>
                        <D:resourcetype/>
                    </D:prop>
                    <D:status>HTTP/1.1 200 OK</D:status>
                </D:propstat>
            </D:response>
        </D:multistatus>"#;

        let items = parse_propfind_xml(sample);
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].href, "/files/myuser/docs/");
        assert!(items[0].is_dir);
        assert_eq!(items[1].href, "/files/myuser/docs/notes.txt");
        assert!(!items[1].is_dir);
        assert_eq!(items[1].size, 1024);
    }

    #[test]
    fn test_resolve_item_url() {
        let base = "https://example.com/remote.php/webdav/folder/";
        assert_eq!(
            resolve_item_url(base, "/remote.php/webdav/folder/file.txt"),
            "https://example.com/remote.php/webdav/folder/file.txt"
        );
        assert_eq!(
            resolve_item_url(base, "sub/file.txt"),
            "https://example.com/remote.php/webdav/folder/sub/file.txt"
        );
        assert_eq!(
            resolve_item_url(base, "https://example.com/remote.php/webdav/folder/file.txt"),
            "https://example.com/remote.php/webdav/folder/file.txt"
        );
    }

    #[test]
    fn test_relative_item_path() {
        let base = "https://example.com/remote.php/webdav/folder/";
        assert_eq!(relative_item_path(base, "/remote.php/webdav/folder/"), "");
        assert_eq!(relative_item_path(base, "/remote.php/webdav/folder/file.txt"), "file.txt");
        assert_eq!(relative_item_path(base, "/remote.php/webdav/folder/sub/data.csv"), "sub/data.csv");
        assert_eq!(relative_item_path(base, "https://example.com/remote.php/webdav/folder/sub/data.csv"), "sub/data.csv");
        assert_eq!(relative_item_path(base, "/remote.php/webdav/folder/my%20folder/file%201.txt"), "my folder/file 1.txt");

        let root_base = "http://localhost:3923/";
        assert_eq!(relative_item_path(root_base, "/"), "");
        assert_eq!(relative_item_path(root_base, "/file.txt"), "file.txt");
        assert_eq!(relative_item_path(root_base, "http://localhost:3923/"), "");
        assert_eq!(relative_item_path(root_base, "http://localhost:3923/file.txt"), "file.txt");
    }

    #[test]
    fn test_build_file_url_spaces() {
        let base = "http://localhost:3923/docs/";
        let url = build_file_url(base, "my folder/my file.txt");
        assert_eq!(url, "http://localhost:3923/docs/my%20folder/my%20file.txt");
        assert!(rustydav::prelude::Url::parse(&url).is_ok());
    }

    #[test]
    fn test_parse_propfind_iis_cases_and_cdata() {
        let xml = r#"<?xml version="1.0" encoding="utf-8"?>
        <D:multistatus xmlns:D="DAV:">
            <D:Response>
                <D:Href><![CDATA[/content/enforced/1052175-dev_asteve18/]]></D:Href>
                <D:PropStat>
                    <D:Prop>
                        <D:ResourceType><D:Collection/></D:ResourceType>
                        <D:iscollection>1</D:iscollection>
                    </D:Prop>
                    <D:Status>HTTP/1.1 200 OK</D:Status>
                </D:PropStat>
            </D:Response>
            <D:Response>
                <D:Href>/content/enforced/1052175-dev_asteve18/lecture1.pdf</D:Href>
                <D:PropStat>
                    <D:Prop>
                        <D:GetContentLength>54321</D:GetContentLength>
                        <D:ResourceType/>
                        <D:iscollection>0</D:iscollection>
                    </D:Prop>
                    <D:Status>HTTP/1.1 200 OK</D:Status>
                </D:PropStat>
            </D:Response>
            <D:Response>
                <D:Href>/content/enforced/1052175-dev_asteve18/assignments/</D:Href>
                <D:PropStat>
                    <D:Prop>
                        <D:isfolder>true</D:isfolder>
                    </D:Prop>
                    <D:Status>HTTP/1.1 200 OK</D:Status>
                </D:PropStat>
            </D:Response>
        </D:multistatus>"#;

        let items = parse_propfind_xml(xml);
        assert_eq!(items.len(), 3);
        assert_eq!(items[0].href, "/content/enforced/1052175-dev_asteve18/");
        assert!(items[0].is_dir);
        assert_eq!(items[1].href, "/content/enforced/1052175-dev_asteve18/lecture1.pdf");
        assert!(!items[1].is_dir);
        assert_eq!(items[1].size, 54321);
        assert_eq!(items[2].href, "/content/enforced/1052175-dev_asteve18/assignments/");
        assert!(items[2].is_dir);
    }

    #[test]
    fn test_relative_item_path_case_insensitivity_and_ports() {
        let base = "https://courselinkdav.desire2learn.com/content/enforced/1052175-dev_asteve18/";

        // 1. Root collection matches case-insensitively
        assert_eq!(
            relative_item_path(base, "/Content/Enforced/1052175-dev_asteve18/"),
            ""
        );

        // 2. File with mixed case prefix
        assert_eq!(
            relative_item_path(base, "/Content/Enforced/1052175-dev_asteve18/syllabus.pdf"),
            "syllabus.pdf"
        );

        // 3. Nested file with mixed case
        assert_eq!(
            relative_item_path(
                base,
                "/Content/Enforced/1052175-dev_asteve18/Week 1/Lecture Notes.pdf"
            ),
            "Week 1/Lecture Notes.pdf"
        );

        // 4. Server href containing explicit port :443
        assert_eq!(
            relative_item_path(
                base,
                "https://courselinkdav.desire2learn.com:443/content/enforced/1052175-dev_asteve18/exam.pdf"
            ),
            "exam.pdf"
        );
    }

    #[test]
    fn test_parse_propfind_d2l_brightspace_real_response() {
        let xml = r#"<?xml version="1.0" encoding="utf-8"?>
<d:multistatus xmlns:d="DAV:">
        <d:response>
                <d:href><![CDATA[/content/enforced/1052175-dev_asteve18/]]></d:href>
                <d:propstat>
                        <d:prop>
                                <d:getlastmodified>Mon, 28 Sep 2026 13:45:09 GMT</d:getlastmodified>
                                <d:resourcetype>
                                        <d:collection/>
                                </d:resourcetype>
                                <d:supportedlock/>
                        </d:prop>
                        <d:status>HTTP/1.1 200 OK</d:status>
                </d:propstat>
        </d:response>
        <d:response>
                <d:href><![CDATA[/content/enforced/1052175-dev_asteve18/.gemini/]]></d:href>
                <d:propstat>
                        <d:prop>
                                <d:getlastmodified>Wed, 24 Jun 2026 13:10:24 GMT</d:getlastmodified>
                                <d:resourcetype>
                                        <d:collection/>
                                </d:resourcetype>
                                <d:supportedlock/>
                        </d:prop>
                        <d:status>HTTP/1.1 200 OK</d:status>
                </d:propstat>
        </d:response>
        <d:response>
                <d:href><![CDATA[/content/enforced/1052175-dev_asteve18/.gitignore]]></d:href>
                <d:propstat>
                        <d:prop>
                                <d:getcontentlength>197</d:getcontentlength>
                                <d:getlastmodified>Wed, 24 Jun 2026 13:09:15 GMT</d:getlastmodified>
                                <d:resourcetype/>
                                <d:getcontenttype>application/octet-stream</d:getcontenttype>
                                <d:supportedlock/>
                        </d:prop>
                        <d:status>HTTP/1.1 200 OK</d:status>
                </d:propstat>
        </d:response>
        <d:response>
                <d:href><![CDATA[/content/enforced/1052175-dev_asteve18/after%20quiz%20and%20survey.html]]></d:href>
                <d:propstat>
                        <d:prop>
                                <d:getcontentlength>120</d:getcontentlength>
                                <d:getlastmodified>Mon, 28 Sep 2026 13:45:09 GMT</d:getlastmodified>
                                <d:resourcetype/>
                                <d:getcontenttype>text/html</d:getcontenttype>
                                <d:supportedlock/>
                        </d:prop>
                        <d:status>HTTP/1.1 200 OK</d:status>
                </d:propstat>
        </d:response>
        <d:response>
                <d:href><![CDATA[/content/enforced/1052175-dev_asteve18/Unit03_MATH1060DE_S26.docx]]></d:href>
                <d:propstat>
                        <d:prop>
                                <d:getcontentlength>3952509</d:getcontentlength>
                                <d:getlastmodified>Tue, 07 Jul 2026 18:13:47 GMT</d:getlastmodified>
                                <d:resourcetype/>
                                <d:getcontenttype>application/vnd.openxmlformats-officedocument.wordprocessingml.document</d:getcontenttype>
                                <d:supportedlock/>
                        </d:prop>
                        <d:status>HTTP/1.1 200 OK</d:status>
                </d:propstat>
        </d:response>
</d:multistatus>"#;

        let base = "https://courselinkdav.desire2learn.com/content/enforced/1052175-dev_asteve18/";
        let items = parse_propfind_xml(xml);
        assert_eq!(items.len(), 5);

        // Item 0: Root collection
        assert_eq!(items[0].href, "/content/enforced/1052175-dev_asteve18/");
        assert!(items[0].is_dir);
        assert_eq!(relative_item_path(base, &items[0].href), "");

        // Item 1: .gemini/ directory
        assert_eq!(items[1].href, "/content/enforced/1052175-dev_asteve18/.gemini/");
        assert!(items[1].is_dir);
        assert_eq!(relative_item_path(base, &items[1].href), ".gemini");

        // Item 2: .gitignore file
        assert_eq!(items[2].href, "/content/enforced/1052175-dev_asteve18/.gitignore");
        assert!(!items[2].is_dir);
        assert_eq!(items[2].size, 197);
        assert_eq!(relative_item_path(base, &items[2].href), ".gitignore");

        // Item 3: after quiz and survey.html (percent-encoded in CDATA)
        assert_eq!(items[3].href, "/content/enforced/1052175-dev_asteve18/after%20quiz%20and%20survey.html");
        assert!(!items[3].is_dir);
        assert_eq!(items[3].size, 120);
        assert_eq!(relative_item_path(base, &items[3].href), "after quiz and survey.html");

        // Item 4: Unit03_MATH1060DE_S26.docx (large file size)
        assert_eq!(items[4].href, "/content/enforced/1052175-dev_asteve18/Unit03_MATH1060DE_S26.docx");
        assert!(!items[4].is_dir);
        assert_eq!(items[4].size, 3952509);
        assert_eq!(relative_item_path(base, &items[4].href), "Unit03_MATH1060DE_S26.docx");

        // Verify mtime parsing on items
        assert!(items[2].mtime.is_some());
        assert!(items[3].mtime.is_some());
        assert!(items[4].mtime.is_some());
    }

    #[test]
    fn test_parse_webdav_date() {
        // Standard RFC 2822 / RFC 1123 HTTP-date
        let parsed = parse_webdav_date("Wed, 24 Jun 2026 13:09:15 GMT");
        assert!(parsed.is_some());
        assert!(parsed.unwrap() > 0);

        // Another RFC 2822 date
        let parsed2 = parse_webdav_date("Mon, 28 Sep 2026 13:45:09 GMT");
        assert!(parsed2.is_some());
        assert!(parsed2.unwrap() > parsed.unwrap());

        // ISO 8601 / RFC 3339 format
        let parsed_iso = parse_webdav_date("2026-09-28T13:45:09Z");
        assert!(parsed_iso.is_some());
        assert_eq!(parsed_iso.unwrap(), parsed2.unwrap());

        // Invalid or empty date
        assert!(parse_webdav_date("").is_none());
        assert!(parse_webdav_date("invalid date string").is_none());
    }

    #[test]
    fn test_should_upload_file() {
        // 1. Different sizes -> must upload regardless of timestamps
        assert!(should_upload_file(100, Some(1000), 200, Some(1000)));
        assert!(should_upload_file(100, None, 200, None));

        // 2. Matching sizes, but local file is newer than remote mtime + 1s -> must upload
        assert!(should_upload_file(100, Some(1005), 100, Some(1000)));

        // 3. Matching sizes, local file is older or equal -> skip (false)
        assert!(!should_upload_file(100, Some(1000), 100, Some(1000)));
        assert!(!should_upload_file(100, Some(999), 100, Some(1000)));

        // 4. Matching sizes, local file is only 1s ahead (within rounding margin) -> skip (false)
        assert!(!should_upload_file(100, Some(1001), 100, Some(1000)));

        // 5. Matching sizes, timestamps missing -> skip (false)
        assert!(!should_upload_file(100, None, 100, Some(1000)));
        assert!(!should_upload_file(100, Some(1000), 100, None));
        assert!(!should_upload_file(100, None, 100, None));
    }

    #[test]
    fn test_relative_path_traversal_guards() {
        assert!(is_safe_relative_path("file.txt"));
        assert!(is_safe_relative_path("sub/file.txt"));
        assert!(is_safe_relative_path("sub/dir/nested.txt"));

        assert!(!is_safe_relative_path(""));
        assert!(!is_safe_relative_path("/file.txt"));
        assert!(!is_safe_relative_path("../file.txt"));
        assert!(!is_safe_relative_path("sub/../file.txt"));
        assert!(!is_safe_relative_path("./file.txt"));
        assert!(!is_safe_relative_path("sub/./file.txt"));
        assert!(!is_safe_relative_path("sub\\file.txt"));

        let base = std::env::temp_dir();
        assert!(safe_join_path(&base, "safe/doc.txt").is_ok());
        assert!(safe_join_path(&base, "../escape.txt").is_err());
        assert!(safe_join_path(&base, "/etc/passwd").is_err());
    }

    #[test]
    fn test_validate_webdav_url_rules() {
        // In production mode (allow_insecure = false):
        // 1. HTTP is rejected
        assert!(validate_webdav_url_internal("http://example.com/dav", false).is_err());
        // 2. Localhost is rejected
        assert!(validate_webdav_url_internal("https://localhost/dav", false).is_err());
        assert!(validate_webdav_url_internal("https://my.localhost/dav", false).is_err());
        // 3. Loopback IP is rejected
        assert!(validate_webdav_url_internal("https://127.0.0.1:3923/", false).is_err());
        // 4. Cloud metadata / link-local is rejected
        assert!(validate_webdav_url_internal("https://169.254.169.254/latest/meta-data", false).is_err());
        // 5. Private RFC 1918 IPs are rejected
        assert!(validate_webdav_url_internal("https://10.0.0.1/dav", false).is_err());
        assert!(validate_webdav_url_internal("https://192.168.1.1/dav", false).is_err());
        assert!(validate_webdav_url_internal("https://172.16.0.1/dav", false).is_err());
        // 6. Valid external HTTPS URL is accepted
        assert!(validate_webdav_url_internal("https://courselinkdav.desire2learn.com/dav", false).is_ok());

        // In test / dev mode (allow_insecure = true):
        assert!(validate_webdav_url_internal("http://127.0.0.1:3923/content/", true).is_ok());
    }

    #[test]
    fn test_symlinks_skipped_in_collect_local_files() {
        let temp_dir = std::env::temp_dir().join(format!("scs_symlink_test_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&temp_dir);
        std::fs::create_dir_all(&temp_dir).unwrap();
        let target_file = temp_dir.join("real_file.txt");
        std::fs::write(&target_file, "real content").unwrap();

        let secret_dir = std::env::temp_dir().join(format!("scs_secret_test_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&secret_dir);
        std::fs::create_dir_all(&secret_dir).unwrap();
        let secret_file = secret_dir.join("secret.txt");
        std::fs::write(&secret_file, "secret content").unwrap();

        #[cfg(unix)]
        {
            let symlink_path = temp_dir.join("symlink_to_secret.txt");
            let _ = std::os::unix::fs::symlink(&secret_file, &symlink_path);
        }

        let collected = collect_local_files_with_mtime(&temp_dir);
        assert_eq!(collected.len(), 1);
        assert_eq!(collected[0].1, "real_file.txt");

        let _ = std::fs::remove_dir_all(&temp_dir);
        let _ = std::fs::remove_dir_all(&secret_dir);
    }

    #[test]
    fn test_webdav_item_builder() {
        let item = WebdavItem::new("/path/file.txt", false, 1234);
        assert_eq!(item.href, "/path/file.txt");
        assert!(!item.is_dir);
        assert_eq!(item.size, 1234);
        assert_eq!(item.mtime, None);

        let item_with_mtime = item.with_mtime(Some(99999));
        assert_eq!(item_with_mtime.mtime, Some(99999));
    }

    #[test]
    fn test_compute_file_sha256() {
        let temp_file = std::env::temp_dir().join(format!("scs_sha_test_{}.txt", std::process::id()));
        std::fs::write(&temp_file, b"test payload for streaming sha256").unwrap();
        let computed = compute_file_sha256(&temp_file).unwrap();
        let direct = compute_sha256(b"test payload for streaming sha256");
        assert_eq!(computed, direct);
        let _ = std::fs::remove_file(&temp_file);
    }

    #[test]
    fn test_get_scan_concurrency_defaults() {
        assert_eq!(DEFAULT_SCAN_CONCURRENCY, 6);
        assert_eq!(MAX_SCAN_CONCURRENCY, 64);
        assert_eq!(MAX_SCANNED_DIRS_LIMIT, 10_000);
        let concurrency = get_scan_concurrency();
        assert!(concurrency >= 1 && concurrency <= MAX_SCAN_CONCURRENCY);
    }

    #[test]
    fn test_remote_cache_lifecycle() {
        let temp_cache = std::env::temp_dir().join(format!("scs_cache_test_{}.json", std::process::id()));
        std::env::set_var("WEBDAV_CACHE_FILE", &temp_cache);

        let test_url = "https://example.com/remote/files/";
        let alice_user = Some("alice");
        let bob_user = Some("bob");

        let items = vec![
            WebdavItem::new("https://example.com/remote/files/doc.pdf", false, 4096),
            WebdavItem::new("https://example.com/remote/files/.gitignore", false, 128),
        ];

        // Save under alice
        save_remote_cache(test_url, alice_user, &items);
        let loaded_alice = load_remote_cache(test_url, alice_user).expect("Cache should load alice's items");
        assert_eq!(loaded_alice.len(), 2);
        assert_eq!(loaded_alice[0].size, 4096);
        // Hidden files like .gitignore must be preserved
        assert!(loaded_alice.iter().any(|i| i.href.ends_with(".gitignore")));

        // Multi-credential isolation: Bob querying the same URL should find NO cached items
        let loaded_bob = load_remote_cache(test_url, bob_user);
        assert!(loaded_bob.is_none(), "Bob should not see Alice's cached items");

        // Update item in alice's cache
        update_remote_cache_item(test_url, alice_user, "doc.pdf", 8192, Some(12345678));
        let updated = load_remote_cache(test_url, alice_user).unwrap();
        let doc = updated.iter().find(|i| i.href.ends_with("doc.pdf")).unwrap();
        assert_eq!(doc.size, 8192);
        assert_eq!(doc.mtime, Some(12345678));

        // Remove item from alice's cache
        remove_remote_cache_item(test_url, alice_user, "doc.pdf");
        let after_removal = load_remote_cache(test_url, alice_user).unwrap();
        assert_eq!(after_removal.len(), 1);
        assert!(after_removal[0].href.ends_with(".gitignore"));

        // Path traversal rejection in cache
        let malicious_items = vec![
            WebdavItem::new("https://example.com/remote/files/../../etc/passwd", false, 100),
            WebdavItem::new("https://example.com/remote/files/%2e%2e/shadow", false, 100),
            WebdavItem::new("https://example.com/remote/files/valid.txt", false, 200),
        ];
        save_remote_cache(test_url, alice_user, &malicious_items);
        let safe_loaded = load_remote_cache(test_url, alice_user).unwrap();
        assert_eq!(safe_loaded.len(), 1);
        assert_eq!(safe_loaded[0].size, 200);

        // Invalid rel_path updates and removals are safely ignored
        update_remote_cache_item(test_url, alice_user, "../malicious", 500, None);
        remove_remote_cache_item(test_url, alice_user, "../malicious");
        let still_safe = load_remote_cache(test_url, alice_user).unwrap();
        assert_eq!(still_safe.len(), 1);

        // Verify traversal sequence helper
        assert!(has_traversal_sequence("../secret"));
        assert!(has_traversal_sequence("foo/../bar"));
        assert!(has_traversal_sequence("foo/%2e%2e/bar"));
        assert!(has_traversal_sequence("foo\\bar"));
        assert!(!has_traversal_sequence(".gitignore"));
        assert!(!has_traversal_sequence(".env"));
        assert!(!has_traversal_sequence("subdir/.hidden_file"));

        clear_remote_cache().unwrap();
        assert!(!temp_cache.exists());
        std::env::remove_var("WEBDAV_CACHE_FILE");
    }

    #[test]
    fn test_should_download_file_decision_matrix() {
        // Different sizes -> download
        assert!(should_download_file(200, Some(1000), 100, Some(1000)));
        assert!(should_download_file(200, None, 100, None));

        // Same size, remote newer than local + 1s -> download
        assert!(should_download_file(100, Some(1005), 100, Some(1000)));

        // Same size, remote older or within 1s margin -> skip
        assert!(!should_download_file(100, Some(1000), 100, Some(1000)));
        assert!(!should_download_file(100, Some(1001), 100, Some(1000)));
        assert!(!should_download_file(100, Some(990), 100, Some(1000)));

        // Missing timestamp -> skip if sizes match
        assert!(!should_download_file(100, None, 100, Some(1000)));
        assert!(!should_download_file(100, Some(1000), 100, None));
    }
}

