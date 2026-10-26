use crate::dav_client::{self, Client};
use std::collections::{HashMap, HashSet, VecDeque};
use std::error::Error as StdError;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{mpsc, Arc, Condvar, Mutex, Weak};
use std::time::{Duration, Instant};

pub const DEFAULT_MAX_FILE_SIZE: u64 = 500 * 1024 * 1024; // 500 MB
/// Default worker concurrency for remote folder scanning and uploads.
pub const DEFAULT_SCAN_CONCURRENCY: usize = 6;
/// Maximum worker concurrency for remote folder scanning and uploads, to prevent overwhelming the server.
pub const MAX_SCAN_CONCURRENCY: usize = 64;
/// Safety limit on the number of traversed directories to guard against recursive symlink bombs or infinite trees.
/// Set to 10,000 to comfortably accommodate very large course hierarchies (typical max depth ~10 * breadth ~100)
/// while bounding memory usage and avoiding infinite traversal cycles.
pub const MAX_SCANNED_DIRS_LIMIT: usize = 10_000;
/// Maximum number of excluded folder patterns applied to a single action.
pub const MAX_EXCLUDE_PATTERNS: usize = 64;
/// Maximum length (in characters) of one excluded folder pattern.
pub const MAX_EXCLUDE_PATTERN_CHARS: usize = 256;
/// Default number of attempts for a single WebDAV request before it is treated as failed.
/// Retrying absorbs transient transport failures (timeouts, connection resets, truncated
/// bodies) so one flaky request cannot abort an entire multi-file transfer.
pub const REQUEST_MAX_ATTEMPTS: u32 = 3;
/// Base delay before the first retry; each subsequent retry multiplies it by the attempt number.
pub const RETRY_BASE_DELAY_MS: u64 = 1_000;
/// Upper bound for a single retry delay.
pub const RETRY_MAX_DELAY_MS: u64 = 10_000;
/// Retry delay used by production call sites; unit tests pass [`Duration::ZERO`] instead.
const RETRY_DELAY: Duration = Duration::from_millis(RETRY_BASE_DELAY_MS);

// Static compile-time assertion verifying that dav_client::Client implements Send + Sync
// and can safely be shared across concurrent scanning worker threads.
// Note: dav_client::Client internally wraps reqwest::blocking::Client, which maintains
// an Arc-backed connection pool designed for concurrent multi-threaded usage.
const _: fn() = || {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<Client>();
};

/// Classification of a single WebDAV request attempt, used to decide whether a retry is worth it.
enum AttemptError {
    /// Transport-level failure (timeout, connection reset, truncated body) that is safe to retry.
    Transient(String),
    /// Deterministic failure (bad credentials, forbidden, local disk error) that a retry cannot fix.
    Fatal(String),
}

/// Formats a WebDAV transport error with its full source chain and a retry
/// classification, then labels it transient or fatal. An unparseable request
/// URL is deterministic, so it is fatal.
fn classify_dav_error(context: &str, error: &dav_client::Error) -> AttemptError {
    match error {
        dav_client::Error::InvalidUrl(msg) => {
            AttemptError::Fatal(format!("{context}: invalid request URL: {msg}"))
        }
        dav_client::Error::Transport(e) => classify_transport_error(context, e),
    }
}

/// Formats a reqwest transport error with its full source chain and a retry classification,
/// then labels it transient or fatal.
fn classify_transport_error(context: &str, error: &reqwest::Error) -> AttemptError {
    let mut detail = format!("{context}: {error}");
    if let Some(url) = error.url() {
        detail.push_str(&format!("\n    url: {url}"));
    }
    if let Some(status) = error.status() {
        detail.push_str(&format!("\n    http status: {status}"));
    }
    detail.push_str(&format!(
        "\n    flags: timeout={} connect={} request={} body={} decode={}",
        error.is_timeout(),
        error.is_connect(),
        error.is_request(),
        error.is_body(),
        error.is_decode(),
    ));
    let mut source = StdError::source(error);
    let mut depth = 0;
    while let Some(cause) = source {
        detail.push_str(&format!("\n    caused by[{depth}]: {cause}"));
        source = cause.source();
        depth += 1;
    }
    let transient =
        error.is_timeout() || error.is_connect() || error.is_request() || error.is_body();
    if transient {
        AttemptError::Transient(detail)
    } else {
        AttemptError::Fatal(detail)
    }
}

/// Formats an I/O error raised while streaming a response body. When the error wraps a reqwest
/// failure (as `reqwest::blocking::Response` does) its transport classification wins, so a
/// mid-download timeout is retried instead of being mistaken for a local disk failure.
fn classify_io_error(context: &str, error: &std::io::Error) -> AttemptError {
    if let Some(inner) = error.get_ref() {
        if let Some(reqwest_error) = inner.downcast_ref::<reqwest::Error>() {
            return classify_transport_error(context, reqwest_error);
        }
    }
    let detail = format!("{context}: {error}");
    let transient = matches!(
        error.kind(),
        std::io::ErrorKind::TimedOut
            | std::io::ErrorKind::Interrupted
            | std::io::ErrorKind::WouldBlock
            | std::io::ErrorKind::UnexpectedEof
            | std::io::ErrorKind::ConnectionReset
            | std::io::ErrorKind::ConnectionAborted
            | std::io::ErrorKind::BrokenPipe
    );
    if transient {
        AttemptError::Transient(detail)
    } else {
        AttemptError::Fatal(detail)
    }
}

/// Returns the delay before retry number `attempt` (1-based): 1x, 2x, 3x ... the base delay, capped.
fn retry_delay(attempt: u32, base: Duration) -> Duration {
    let millis = base
        .as_millis()
        .saturating_mul(u128::from(attempt.max(1)))
        .min(u128::from(RETRY_MAX_DELAY_MS));
    Duration::from_millis(millis as u64)
}

/// Sleeps for `total`, waking every 100 ms so a cancel request is honored promptly.
fn sleep_cancellable(total: Duration, cancel_flag: &AtomicBool) {
    let step = Duration::from_millis(100);
    let mut remaining = total;
    while !remaining.is_zero() {
        if cancel_flag.load(Ordering::SeqCst) {
            return;
        }
        let slice = remaining.min(step);
        std::thread::sleep(slice);
        remaining = remaining.saturating_sub(slice);
    }
}

/// Runs one WebDAV operation with bounded retries and verbose per-attempt diagnostics.
///
/// Only transport-level [`AttemptError::Transient`] failures are retried (timeouts, connection
/// resets, truncated bodies); permanent failures return immediately. When every attempt is
/// exhausted the last failure is returned as `Err` so the caller can decide whether to skip the
/// item and keep going or fail the whole action.
fn retry_webdav_operation<T, F, L>(
    operation: &str,
    cancel_flag: &AtomicBool,
    max_attempts: u32,
    base_delay: Duration,
    mut log: L,
    mut attempt_op: F,
) -> Result<T, String>
where
    F: FnMut() -> Result<T, AttemptError>,
    L: FnMut(&str),
{
    let max_attempts = max_attempts.max(1);
    let mut last_failure = String::new();
    for attempt in 1..=max_attempts {
        if cancel_flag.load(Ordering::SeqCst) {
            return Err("Operation canceled by user.".to_string());
        }
        let started = Instant::now();
        match attempt_op() {
            Ok(value) => {
                if attempt > 1 {
                    log(&format!(
                        "[webdav] {operation}: recovered on attempt {attempt}/{max_attempts} after {:?}.\n",
                        started.elapsed()
                    ));
                }
                return Ok(value);
            }
            Err(AttemptError::Fatal(detail)) => {
                log(&format!(
                    "[webdav] {operation}: attempt {attempt}/{max_attempts} failed permanently after {:?}; not retrying.\n{detail}\n",
                    started.elapsed()
                ));
                return Err(detail);
            }
            Err(AttemptError::Transient(detail)) => {
                log(&format!(
                    "[webdav] {operation}: attempt {attempt}/{max_attempts} hit a transient transport failure after {:?}.\n{detail}\n",
                    started.elapsed()
                ));
                last_failure = detail;
                if attempt == max_attempts {
                    break;
                }
                let delay = retry_delay(attempt, base_delay);
                log(&format!(
                    "[webdav] {operation}: retrying in {delay:?} (attempt {}/{max_attempts})...\n",
                    attempt + 1
                ));
                sleep_cancellable(delay, cancel_flag);
                if cancel_flag.load(Ordering::SeqCst) {
                    return Err("Operation canceled by user.".to_string());
                }
            }
        }
    }
    log(&format!(
        "[webdav] {operation}: giving up after {max_attempts} attempt(s).\n"
    ));
    Err(last_failure)
}

/// Normalizes one user-supplied excluded folder pattern into a slash-separated path.
///
/// Leading/trailing slashes, `.` segments, backslashes, and repeated separators are
/// dropped, so `.git/`, `/.git`, and `.git` all normalize to `.git`. Patterns that are
/// empty, escape with `..`, contain control characters, or exceed
/// [`MAX_EXCLUDE_PATTERN_CHARS`] are rejected. Matching itself is relative to the scan
/// root, so a normalized pattern can never address anything outside the selected remote
/// subdirectory.
pub fn normalize_exclude_pattern(raw: &str) -> Option<String> {
    let replaced = raw.trim().replace('\\', "/");
    let parts: Vec<&str> = replaced
        .split('/')
        .filter(|part| !part.is_empty() && *part != ".")
        .collect();
    if parts
        .iter()
        .any(|part| *part == ".." || part.chars().any(|c| c.is_control()))
    {
        return None;
    }
    let normalized = parts.join("/");
    if normalized.is_empty() || normalized.chars().count() > MAX_EXCLUDE_PATTERN_CHARS {
        None
    } else {
        Some(normalized)
    }
}

/// Normalizes, validates, and deduplicates exclusion patterns while preserving their order.
pub fn normalize_exclude_folders(raw: &[String]) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut normalized = Vec::new();
    for entry in raw {
        if normalized.len() >= MAX_EXCLUDE_PATTERNS {
            break;
        }
        if let Some(pattern) = normalize_exclude_pattern(entry) {
            if seen.insert(pattern.clone()) {
                normalized.push(pattern);
            }
        }
    }
    normalized
}

/// Returns true when a scan-root-relative path lives in an excluded folder.
///
/// - A pattern containing `/` matches that relative folder and everything under it, so
///   `assets/cache` excludes `assets/cache/` and `assets/cache/data.bin`.
/// - A pattern without `/` matches that folder name at any depth, so `.git` excludes
///   `.git/`, `sub/.git/`, and their contents.
///
/// `is_dir` distinguishes the path itself from its descendants: a *file* named `.git`
/// is not an excluded folder, while a file under `.git/` still is. Matching is
/// case-sensitive and only considers path components: it never resolves the path, so
/// it cannot be used to escape the scan root.
pub fn is_excluded_path(rel_path: &str, is_dir: bool, exclude_folders: &[String]) -> bool {
    if exclude_folders.is_empty() {
        return false;
    }
    let normalized = rel_path.trim_matches('/').replace('\\', "/");
    if normalized.is_empty() {
        return false;
    }
    let segments: Vec<&str> = normalized.split('/').collect();
    exclude_folders.iter().any(|pattern| {
        if pattern.contains('/') {
            if normalized == *pattern {
                return is_dir;
            }
            normalized
                .strip_prefix(pattern.as_str())
                .is_some_and(|rest| rest.starts_with('/'))
        } else {
            segments.iter().enumerate().any(|(index, segment)| {
                // An intermediate matching component is always a directory; the final
                // component only counts when the path itself is a directory.
                *segment == pattern && (is_dir || index + 1 < segments.len())
            })
        }
    })
}

/// Returns the environment-configured default concurrency limit.
pub fn get_scan_concurrency() -> usize {
    std::env::var("WEBDAV_SCAN_CONCURRENCY")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .map(|v| v.clamp(1, MAX_SCAN_CONCURRENCY))
        .unwrap_or(DEFAULT_SCAN_CONCURRENCY)
}

/// Live worker limit shared by scans and uploads. `0` means "not set yet", so the
/// environment-configured default applies until the user changes the setting.
static LIVE_SCAN_CONCURRENCY: AtomicUsize = AtomicUsize::new(0);

/// Cancellation and live-limit notification for one in-flight worker pool.
///
/// The closure locks the pool's own state mutex before calling `notify_all`, so a
/// worker cannot check its wait condition and park in the gap before the notify.
type PoolWake = dyn Fn() + Send + Sync;

/// Wakers for every in-flight worker pool, so settings changes and cancellation
/// can reach workers that are already parked.
static CONCURRENCY_WAKEUPS: Mutex<Vec<Weak<PoolWake>>> = Mutex::new(Vec::new());

/// Returns the live concurrency limit, falling back to the environment default.
pub fn current_scan_concurrency() -> usize {
    match LIVE_SCAN_CONCURRENCY.load(Ordering::Relaxed) {
        0 => get_scan_concurrency(),
        value => value.clamp(1, MAX_SCAN_CONCURRENCY),
    }
}

/// Updates the live worker limit and wakes every in-flight worker pool, so a
/// Settings change applies to the operation that is already running.
pub fn set_scan_concurrency(threads: usize) {
    LIVE_SCAN_CONCURRENCY.store(threads.clamp(1, MAX_SCAN_CONCURRENCY), Ordering::SeqCst);
    wake_worker_pools();
}

/// Wakes every in-flight pool so workers re-check cancellation and the live limit.
pub fn wake_worker_pools() {
    let Ok(mut wakeups) = CONCURRENCY_WAKEUPS.lock() else {
        return;
    };
    wakeups.retain(|weak| match weak.upgrade() {
        Some(wake) => {
            wake();
            true
        }
        None => false,
    });
}

/// Registers a pool's waker so [`wake_worker_pools`] can reach it.
fn register_concurrency_wakeup(wake: &Arc<PoolWake>) {
    if let Ok(mut wakeups) = CONCURRENCY_WAKEUPS.lock() {
        wakeups.retain(|weak| weak.upgrade().is_some());
        wakeups.push(Arc::downgrade(wake));
    }
}

/// Retrieves maximum allowed WebDAV file size in bytes, configurable via MAX_WEBDAV_FILE_SIZE_BYTES.
pub fn get_max_file_size() -> u64 {
    std::env::var("MAX_WEBDAV_FILE_SIZE_BYTES")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(DEFAULT_MAX_FILE_SIZE)
}

/// End-to-end deadline for one streamed download body. reqwest's blocking
/// client applies its 30s timeout per individual `read()`, so a body dripped
/// at one byte per 29 seconds never times out; this ceiling bounds the whole
/// transfer and the copy loop also observes the cancel flag every chunk.
pub fn get_download_body_timeout_secs() -> u64 {
    std::env::var("WEBDAV_DOWNLOAD_BODY_TIMEOUT_SECS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .filter(|v| *v > 0)
        .unwrap_or(600)
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
    if let Ok(base) = reqwest::Url::parse(base_url) {
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
    let resolved_url = match reqwest::Url::parse(&resolved_url_str) {
        Ok(u) => u,
        Err(_) => return sanitize_relative_path(&decode_percent(item_href)),
    };
    let base_url_parsed = match reqwest::Url::parse(base_url) {
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

    // 2. Direct prefix match (case-insensitive). `split_at_checked` keeps the
    // slice panic-free when a percent-decoded multi-byte character in the
    // item path straddles the byte length of the base path.
    if trimmed_item.len() > trimmed_base.len() {
        if let Some((prefix, suffix)) = trimmed_item.split_at_checked(trimmed_base.len()) {
            if prefix.eq_ignore_ascii_case(trimmed_base)
                && (suffix.starts_with('/') || trimmed_base.is_empty())
            {
                return sanitize_relative_path(suffix);
            }
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

/// True only when `target_url` shares the scheme/host/port origin of the
/// configured `remote_url`, so a server-supplied href cannot redirect an
/// authenticated request to another host.
fn same_remote_origin(remote_url: &str, target_url: &str) -> bool {
    matches!(
        (
            reqwest::Url::parse(remote_url),
            reqwest::Url::parse(target_url),
        ),
        (Ok(a), Ok(b)) if a.origin() == b.origin()
    )
}

/// Gate for any URL taken from a server response before it becomes a request:
/// it must stay on the configured remote's origin and still pass the SSRF
/// guard. Returns the user-facing reason when the URL must not be used.
///
/// Redirect targets never reach the transport: the in-repo `dav_client`
/// builds its reqwest client with `redirect::Policy::none()`, so a 3xx sent
/// by the remote is returned as a plain non-success response instead of
/// being followed with the Basic-auth header attached.
fn guard_server_derived_url(remote_url: &str, target_url: &str) -> Result<(), String> {
    if !same_remote_origin(remote_url, target_url) {
        return Err("href resolves outside the configured remote origin".to_string());
    }
    validate_webdav_url(target_url)
}

/// Builds the URL for a specific file under a collection, ensuring proper percent-encoding.
pub fn build_file_url(collection_url: &str, rel_path: &str) -> String {
    if let Ok(mut base) = reqwest::Url::parse(collection_url) {
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
    validate_webdav_url_internal(url_str, insecure_webdav_allowed())
}

/// Whether insecure WebDAV origins (plain HTTP, private addresses) are
/// permitted in this process: unit tests, an explicit development override,
/// or a configured live test server.
pub(crate) fn insecure_webdav_allowed() -> bool {
    cfg!(test) || std::env::var("ALLOW_INSECURE_WEBDAV").as_deref() == Ok("1")
}

pub fn validate_webdav_url_internal(url_str: &str, allow_insecure: bool) -> Result<(), String> {
    let url = reqwest::Url::parse(url_str)
        .map_err(|e| format!("Invalid WebDAV URL: {}", e))?;

    // Reject credentials embedded in the URL itself (https://user:pass@host/):
    // the password would be persisted verbatim in the settings store, echoed
    // into operation logs, and attached by the transport regardless of host —
    // outside the keyring vault every other credential path uses.
    if !url.username().is_empty() || url.password().is_some() {
        return Err(
            "URL must not contain embedded credentials: set the username and password separately."
                .to_string(),
        );
    }

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

    // Check IP addresses for private / loopback / link-local / metadata ranges.
    // `host_str()` returns IPv6 hosts bracketed ([::1]), so strip the brackets
    // before parsing — the old bare `host.parse()` never matched IPv6 literals.
    let unbracketed = lower_host.trim_start_matches('[').trim_end_matches(']');
    if let Ok(ip) = unbracketed.parse::<std::net::IpAddr>() {
        if let Some(reason) = blocked_ip_reason(ip) {
            return Err(reason.to_string());
        }
    } else {
        // Resolve DNS names and reject any address in a blocked private range.
        // The residual DNS rebinding TOCTOU (guard resolves, transport
        // re-resolves) is closed at the transport: `dav_client` pins the
        // validated addresses with `resolve_to_addrs`, so connect-time DNS is
        // bypassed entirely.
        check_resolved_host(unbracketed)?;
    }

    Ok(())
}

/// Resolves a hostname and rejects it when any address it maps to is blocked.
///
/// Fails closed: a lookup that cannot complete says nothing about where the
/// request would land, so an unanswered resolution is an error, not a pass.
///
/// Deliberately not memoized, even though the download and delete loops call
/// the guard once per item: a host that validates as public can rebind to a
/// private address moments later, and a cached verdict would leave the guard
/// switched off for that host for the rest of the process. Paying one blocking
/// lookup per item keeps every request checked against current DNS.
fn check_resolved_host(host: &str) -> Result<(), String> {
    use std::net::ToSocketAddrs;

    let resolution_target = format!("{host}:0");
    let addrs = resolution_target.to_socket_addrs().map_err(|e| {
        format!("Could not resolve WebDAV host {host} ({e}); refusing to connect.")
    })?;
    let mut resolved_any = false;
    for addr in addrs {
        resolved_any = true;
        if let Some(reason) = blocked_ip_reason(addr.ip()) {
            return Err(reason.to_string());
        }
    }
    if !resolved_any {
        return Err(format!(
            "Could not resolve WebDAV host {host}; refusing to connect."
        ));
    }
    Ok(())
}

/// Returns the block reason when the IP falls in a range the client must not
/// reach, including when a blocked IPv4 range is spelled as an IPv6 address by
/// a transition mechanism (IPv4-mapped, NAT64, 6to4, Teredo).
pub(crate) fn blocked_ip_reason(ip: std::net::IpAddr) -> Option<&'static str> {
    match ip {
        std::net::IpAddr::V4(ipv4) => {
            if ipv4.is_loopback() {
                return Some("Access to loopback IP is blocked for security.");
            }
            if ipv4.is_link_local() {
                return Some("Access to link-local IP (cloud metadata) is blocked for security.");
            }
            if ipv4.is_private() {
                return Some("Access to private RFC 1918 IP is blocked for security.");
            }
            if ipv4.is_broadcast() || ipv4.is_unspecified() {
                return Some("Access to broadcast/unspecified IP is blocked for security.");
            }
            let octets = ipv4.octets();
            // 100.64.0.0/10 Carrier-grade NAT
            if octets[0] == 100 && (octets[1] & 0xC0) == 64 {
                return Some("Access to shared carrier-grade NAT IP is blocked for security.");
            }
            None
        }
        std::net::IpAddr::V6(ipv6) => {
            if ipv6.is_loopback() {
                return Some("Access to loopback IPv6 is blocked for security.");
            }
            if ipv6.is_unspecified() {
                return Some("Access to unspecified IPv6 is blocked for security.");
            }
            let segments = ipv6.segments();
            // fe80::/10 link-local
            if (segments[0] & 0xffc0) == 0xfe80 {
                return Some("Access to link-local IPv6 is blocked for security.");
            }
            // fc00::/7 unique local address
            if (segments[0] & 0xfe00) == 0xfc00 {
                return Some("Access to unique local IPv6 is blocked for security.");
            }
            // 64:ff9b:1::/48 is local-use NAT64 (RFC 8215): a private
            // translation prefix, never a public WebDAV destination.
            if segments[0] == 0x64 && segments[1] == 0xff9b && segments[2] == 1 {
                return Some("Access to local-use NAT64 IPv6 is blocked for security.");
            }
            // IPv4 addresses carried inside an IPv6 spelling by a transition
            // mechanism must not smuggle a blocked range past this check.
            if let Some(reason) = embedded_ipv4_reason(ipv6) {
                return Some(reason);
            }
            None
        }
    }
}

/// Returns some reason when a blocked IPv4 range is embedded in an IPv6
/// address by a transition mechanism: IPv4-mapped (`::ffff:a.b.c.d`) and
/// IPv4-compatible (`::a.b.c.d`), NAT64 well-known (`64:ff9b::/96`), 6to4
/// (`2002:aabb:ccdd::/48`), and Teredo's server and bit-inverted client
/// addresses (`2001:0::/32`).
fn embedded_ipv4_reason(ipv6: std::net::Ipv6Addr) -> Option<&'static str> {
    let octets = |hi: u16, lo: u16| {
        std::net::Ipv4Addr::new((hi >> 8) as u8, hi as u8, (lo >> 8) as u8, lo as u8)
    };
    let check = |v4: std::net::Ipv4Addr| blocked_ip_reason(std::net::IpAddr::V4(v4));

    if let Some(compatible_or_mapped) = ipv6.to_ipv4() {
        if let Some(reason) = check(compatible_or_mapped) {
            return Some(reason);
        }
    }
    let segments = ipv6.segments();
    // RFC 6052 NAT64: 64:ff9b::/96 with segments 2..=5 zero. The embedded IPv4
    // is the final 32 bits — reading segments 4 and 5 instead decoded every
    // standard NAT64 address as 0.0.0.0.
    if segments[0] == 0x64
        && segments[1] == 0xff9b
        && (segments[2] | segments[3] | segments[4] | segments[5]) == 0
    {
        if let Some(reason) = check(octets(segments[6], segments[7])) {
            return Some(reason);
        }
    }
    // 6to4: 2002:<IPv4 as two segments>::/48.
    if segments[0] == 0x2002 {
        if let Some(reason) = check(octets(segments[1], segments[2])) {
            return Some(reason);
        }
    }
    // Teredo: 2001:0:<server IPv4>:<flags|port>:<client IPv4, inverted>.
    if segments[0] == 0x2001 && segments[1] == 0 {
        if let Some(reason) = check(octets(segments[2], segments[3])) {
            return Some(reason);
        }
        let client = !((u32::from(segments[6]) << 16) | u32::from(segments[7]));
        if let Some(reason) = check(std::net::Ipv4Addr::from(client)) {
            return Some(reason);
        }
    }
    None
}

/// Ensures all parent collections exist for a relative file path prior to PUT.
///
/// Each MKCOL is retried on transient transport failures; the streamed logs describe every
/// attempt. A failure that survives all retries is returned to the caller, which logs it and
/// skips only the affected file.
pub fn ensure_remote_parent_dirs<L: FnMut(&str)>(
    client: &Client,
    remote_url: &str,
    rel_path: &str,
    cancel_flag: &AtomicBool,
    log: &mut L,
) -> Result<(), String> {
    validate_webdav_url(remote_url)?;

    if !is_safe_relative_path(rel_path) {
        return Err(format!("Unsafe relative path rejected: {}", rel_path));
    }

    let res = retry_webdav_operation(
        &format!("MKCOL {remote_url}"),
        cancel_flag,
        REQUEST_MAX_ATTEMPTS,
        RETRY_DELAY,
        &mut *log,
        || {
            client
                .mkcol(remote_url)
                .map_err(|e| classify_dav_error(&format!("MKCOL {remote_url}"), &e))
        },
    );
    match res {
        Ok(r) => {
            let status = r.status().as_u16();
            if status != 201 && status != 405 && status != 200 && status != 301 && status != 302 {
                if status == 401 || status == 403 {
                    return Err(format!("MKCOL failed for {}: HTTP {}", remote_url, status));
                }
            }
        }
        Err(e) => {
            return Err(format!("MKCOL failed for {}: {}", remote_url, e));
        }
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
        let res = retry_webdav_operation(
            &format!("MKCOL {dir_url}"),
            cancel_flag,
            REQUEST_MAX_ATTEMPTS,
            RETRY_DELAY,
            &mut *log,
            || {
                client
                    .mkcol(&dir_url)
                    .map_err(|e| classify_dav_error(&format!("MKCOL {dir_url}"), &e))
            },
        )
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
    client: &Client,
    remote_url: &str,
    cancel_flag: &AtomicBool,
) -> Result<Vec<WebdavItem>, String> {
    list_remote_recursive_with_log(client, remote_url, cancel_flag, |_| {})
}

/// Recursively lists remote WebDAV items under remote_url with streamed diagnostic logs.
pub fn list_remote_recursive_with_log<F>(
    client: &Client,
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
/// `Client` implements `Send + Sync` (internally backed by `reqwest::blocking::Client` connection pool)
/// allowing concurrent Depth: 1 requests across worker threads.
///
/// Parameters:
/// - `concurrency`: Worker thread count (1..64); updates the live limit when `Some`.
///   `None` keeps the live limit (the environment default until the user changes it).
/// - `auth_user`: Optional username for credential-scoped cache partitioning (CWE-287 / CWE-384).
pub fn list_remote_recursive_with_concurrency_and_log<F>(
    client: &Client,
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
/// - `concurrency`: Worker thread count (1..64); updates the live limit when `Some`.
///   `None` keeps the live limit (the environment default until the user changes it).
/// - `auth_user`: Optional username for credential-scoped cache partitioning (CWE-287 / CWE-384).
/// - `use_cache`: When true and cache is enabled, loads from/saves to remote listing cache. When false (e.g. GET), bypasses cache.
///
/// This is the exclusion-free variant; [`list_remote_recursive_with_exclusions_and_log`]
/// additionally prunes excluded folders from the traversal.
pub fn list_remote_recursive_with_options_and_log<F>(
    client: &Client,
    remote_url: &str,
    cancel_flag: &AtomicBool,
    concurrency: Option<usize>,
    auth_user: Option<&str>,
    use_cache: bool,
    log: F,
) -> Result<Vec<WebdavItem>, String>
where
    F: FnMut(&str),
{
    list_remote_recursive_with_exclusions_and_log(
        client,
        remote_url,
        cancel_flag,
        concurrency,
        auth_user,
        use_cache,
        &[],
        log,
    )
}

/// Recursively lists remote WebDAV items under remote_url, pruning excluded folders.
///
/// Excluded directories are never enqueued for traversal (so their contents are never
/// requested) and any items under them are dropped from the result — including results
/// of a `Depth: infinity` response, which the server may expand on its own.
///
/// Parameters:
/// - `exclude_folders`: Normalized patterns (see [`normalize_exclude_folders`]).
/// - `use_cache`: Listing cache is bypassed while exclusions are active, because a cached
///   listing cannot be reconciled with patterns added or removed since it was saved.
pub fn list_remote_recursive_with_exclusions_and_log<F>(
    client: &Client,
    remote_url: &str,
    cancel_flag: &AtomicBool,
    concurrency: Option<usize>,
    auth_user: Option<&str>,
    use_cache: bool,
    exclude_folders: &[String],
    mut log: F,
) -> Result<Vec<WebdavItem>, String>
where
    F: FnMut(&str),
{
    validate_webdav_url(remote_url)?;
    if cancel_flag.load(Ordering::SeqCst) {
        return Err("Operation canceled by user.".to_string());
    }
    let cache_requested = use_cache && is_cache_enabled();
    // A cached listing cannot be reconciled with patterns added or removed since it was
    // saved, so only reuse the cache when no exclusions are active.
    let use_cache = use_cache && exclude_folders.is_empty();
    if cache_requested && !use_cache {
        log("Remote listing cache bypassed while folder exclusions are active.\n");
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
    let infinity_result = retry_webdav_operation(
        &format!("PROPFIND Depth: infinity {remote_url}"),
        cancel_flag,
        REQUEST_MAX_ATTEMPTS,
        RETRY_DELAY,
        &mut log,
        || {
            client.list(remote_url, "infinity").map_err(|e| {
                classify_dav_error(&format!("PROPFIND Depth: infinity {remote_url}"), &e)
            })
        },
    );
    match infinity_result {
        Ok(res) => {
            let status = res.status();
            if status.is_success() || status.as_u16() == 207 {
                let body = res.text().unwrap_or_default();
                let items: Vec<WebdavItem> = parse_propfind_xml(&body)
                    .into_iter()
                    // A Depth: infinity response is expanded server-side, so drop excluded
                    // folders and everything under them before they can be cached or used.
                    .filter(|item| {
                        !is_excluded_path(
                            &relative_item_path(remote_url, &item.href),
                            item.is_dir,
                            exclude_folders,
                        )
                    })
                    .collect();
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
            if cancel_flag.load(Ordering::SeqCst) {
                return Err("Operation canceled by user.".to_string());
            }
            log(&format!(
                "Depth: infinity request failed after retries ({}). Falling back to Depth: 1 traversal...\n",
                e
            ));
        }
    }

    // Fallback: Concurrent Breadth-First-Search traversal using Depth: 1
    log("Scanning directories using Depth: 1...\n");

    if let Some(requested) = concurrency {
        set_scan_concurrency(requested);
    }
    let remote_parsed = reqwest::Url::parse(remote_url)
        .map_err(|e| format!("Invalid remote URL: {}", e))?;

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
        /// Directories whose listing kept failing after every retry and were skipped.
        skipped_dirs: Vec<String>,
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
                // Only dispatch while below the live limit; excess workers park here
                // and start as soon as the user raises the Settings "Threads" value.
                if state.active_workers < current_scan_concurrency() {
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
                }
                if state.queue.is_empty() && state.active_workers == 0 {
                    state.stopped = true;
                    cvar_ref.notify_all();
                    return None;
                }
                // Every condition change notifies while holding this mutex, so the
                // wait cannot miss a wakeup and needs no timeout polling.
                state = match cvar_ref.wait(state) {
                    Ok(guard) => guard,
                    Err(poisoned) => {
                        eprintln!("[webdav scan] Mutex poisoned during worker wait; recovering state.");
                        poisoned.into_inner()
                    }
                };
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

    let state_mutex = Arc::new(Mutex::new(ScanState {
        queue: initial_queue,
        active_workers: 0,
        visited: initial_visited,
        seen_items: HashSet::new(),
        all_items: Vec::new(),
        scanned_count: 0,
        skipped_dirs: Vec::new(),
        error: None,
        stopped: false,
    }));
    let cvar = Arc::new(Condvar::new());
    let wake: Arc<PoolWake> = {
        let state = Arc::clone(&state_mutex);
        let cvar = Arc::clone(&cvar);
        Arc::new(move || {
            // Hold the pool lock while notifying so a worker cannot miss this
            // wakeup between checking its condition and parking on the condvar.
            let _held = state.lock().unwrap_or_else(|p| p.into_inner());
            cvar.notify_all();
        })
    };
    register_concurrency_wakeup(&wake);

    let state_ref = state_mutex.as_ref();
    let cvar_ref = cvar.as_ref();

    // Spawns one scan worker; the controller below calls it as the live limit rises.
    #[allow(clippy::too_many_arguments)]
    fn spawn_scan_worker<'scope, 'env: 'scope>(
        scope: &'scope std::thread::Scope<'scope, 'env>,
        client: &'env Client,
        remote_url: &'env str,
        remote_origin: reqwest::Url,
        cancel_flag: &'env AtomicBool,
        state_ref: &'env Mutex<ScanState>,
        cvar_ref: &'env Condvar,
        exclude_folders: &'env [String],
        log_tx: mpsc::Sender<String>,
    ) {
        let worker_log_tx = log_tx;
        let worker_remote_origin = remote_origin.origin();
        scope.spawn(move || {
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

                    let list_res = retry_webdav_operation(
                        &format!("PROPFIND Depth: 1 {current_url}"),
                        cancel_flag,
                        REQUEST_MAX_ATTEMPTS,
                        RETRY_DELAY,
                        |msg: &str| {
                            let _ = worker_log_tx.send(msg.to_string());
                        },
                        || {
                            client.list(&current_url, "1").map_err(|e| {
                                classify_dav_error(
                                    &format!("PROPFIND Depth: 1 {current_url}"),
                                    &e,
                                )
                            })
                        },
                    );

                    // Check cancellation immediately after blocking network I/O returns to abort before parsing response body
                    if cancel_flag.load(Ordering::SeqCst) {
                        let mut state = state_ref.lock().unwrap_or_else(|p| p.into_inner());
                        state.stopped = true;
                        return;
                    }

                    let res = match list_res {
                        Ok(r) => r,
                        Err(e) => {
                            if scan_idx == 1 {
                                // Without the root listing there is nothing to traverse, so the
                                // scan still fails — but only after every retry has been spent.
                                let mut state = state_ref.lock().unwrap_or_else(|p| p.into_inner());
                                state.error = Some(format!(
                                    "Root listing failed for {}: {}",
                                    remote_url, e
                                ));
                                state.stopped = true;
                                return;
                            }
                            // A flaky subfolder must not abort the whole scan: record it, warn
                            // loudly, and let the remaining workers finish the traversal.
                            {
                                let mut state = state_ref.lock().unwrap_or_else(|p| p.into_inner());
                                state.skipped_dirs.push(current_url.clone());
                            }
                            let _ = worker_log_tx.send(format!(
                                "[{}] WARNING: giving up on {}; skipping this directory and continuing the scan. Files inside it are missing from this listing.\n",
                                scan_idx, display_folder
                            ));
                            continue;
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

                        // Resolve once so an href that is relative to the current
                        // collection still yields a correct scan-root-relative path.
                        let item_url = resolve_item_url(&current_url, &item.href);
                        let rel_from_root = relative_item_path(remote_url, &item_url);

                        // Excluded folders are pruned before they are enqueued, so no
                        // request is ever issued for their contents. This also drops
                        // descendants that a single listing may have returned.
                        if is_excluded_path(&rel_from_root, item.is_dir, exclude_folders) {
                            continue;
                        }

                        children_in_dir += 1;
                        if item.is_dir {
                            new_dirs += 1;
                            if let Ok(parsed_sub) = reqwest::Url::parse(&item_url) {
                                if parsed_sub.origin() == worker_remote_origin
                                    && !rel_from_root.is_empty()
                                    && is_safe_relative_path(&rel_from_root)
                                    && validate_webdav_url(&item_url).is_ok()
                                {
                                    discovered_sub_urls.push(item_url);
                                }
                            }
                            discovered_items.push(item);
                        } else {
                            new_files += 1;
                            discovered_items.push(item);
                        }
                    }

                    if children_in_dir == 0 && current_url == remote_url {
                        // Char-based truncation: a fixed byte index can split a
                        // multi-byte UTF-8 code point in a server-controlled body,
                        // which panics and aborts the whole scan.
                        let preview: String = body.chars().take(400).collect();
                        let _ = worker_log_tx.send(format!(
                            "Notice: 0 items parsed from collection listing. Response preview:\n{}\n",
                            preview
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

    let (log_tx, log_rx) = mpsc::channel::<String>();

    std::thread::scope(|s| {
        // One controller thread owns spawning, so the pool starts at the current
        // limit and grows as Settings raises it instead of pre-allocating 64 threads.
        let controller = s.spawn(move || {
            let mut spawned = 0usize;
            loop {
                let mut state = state_ref.lock().unwrap_or_else(|p| p.into_inner());
                if state.stopped || state.error.is_some() {
                    break;
                }
                let target = current_scan_concurrency();
                if spawned < target {
                    spawned += 1;
                    drop(state);
                    spawn_scan_worker(
                        s,
                        client,
                        remote_url,
                        remote_parsed.clone(),
                        cancel_flag,
                        state_ref,
                        cvar_ref,
                        exclude_folders,
                        log_tx.clone(),
                    );
                    continue;
                }
                state = cvar_ref.wait(state).unwrap_or_else(|p| p.into_inner());
            }
        });

        while let Ok(msg) = log_rx.recv() {
            log(&msg);
        }
        let _ = controller.join();
    });

    let mut state = state_ref.lock().unwrap_or_else(|p| p.into_inner());

    if cancel_flag.load(Ordering::SeqCst) {
        return Err("Operation canceled by user.".to_string());
    }

    if let Some(err) = state.error.take() {
        return Err(err);
    }

    state.all_items.sort_by(|a, b| a.href.cmp(&b.href));

    if use_cache && is_cache_enabled() {
        save_remote_cache(remote_url, auth_user, &state.all_items);
    }

    if !state.skipped_dirs.is_empty() {
        log(&format!(
            "\nWARNING: {} director(ies) could not be listed and were skipped; files inside them are absent from this listing (see the [webdav] log above for the failed requests):\n",
            state.skipped_dirs.len()
        ));
        for dir in &state.skipped_dirs {
            log(&format!("  - {}\n", dir));
        }
    }

    let file_count = state.all_items.iter().filter(|i| !i.is_dir).count();
    let dir_count = state.all_items.iter().filter(|i| i.is_dir).count();
    log(&format!(
        "\nScan complete: {} file(s) and {} subdirector(ies) discovered across {} folder(s).\n\n",
        file_count, dir_count, state.scanned_count
    ));

    Ok(std::mem::take(&mut state.all_items))
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
    collect_local_files_with_mtime_excluding(dir, &[])
}

/// Walks a local directory recursively, pruning excluded folders before descending.
///
/// An excluded directory is never read, so files inside it cannot be uploaded, mirrored,
/// or reported as missing by comparison actions. Symlinks are skipped as before.
pub fn collect_local_files_with_mtime_excluding(
    dir: &Path,
    exclude_folders: &[String],
) -> Vec<(PathBuf, String, u64, Option<u64>)> {
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
                    if let Ok(rel) = path.strip_prefix(dir) {
                        let rel_str = rel.to_string_lossy().replace('\\', "/");
                        if is_excluded_path(&rel_str, true, exclude_folders) {
                            continue;
                        }
                    }
                    stack.push(path);
                } else if symlink_meta.is_file() {
                    if let Ok(canonical_path) = path.canonicalize() {
                        if !canonical_path.starts_with(&canonical_base) {
                            continue;
                        }
                    }
                    if let Ok(rel) = path.strip_prefix(dir) {
                        let rel_str = rel.to_string_lossy().replace('\\', "/");
                        if is_excluded_path(&rel_str, false, exclude_folders) {
                            continue;
                        }
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
    collect_local_files_excluding(dir, &[])
}

/// Walks a local directory recursively while pruning excluded folders.
pub fn collect_local_files_excluding(
    dir: &Path,
    exclude_folders: &[String],
) -> Vec<(PathBuf, String, u64)> {
    collect_local_files_with_mtime_excluding(dir, exclude_folders)
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
/// The 1-second margin accommodates RFC 7231 Section 7.1.1.1 HTTP-date 1-second timestamp resolution
/// and slight client/server clock skew between WebDAV hosts. Strict inequality `> l_time + 1`
/// guarantees that the remote modification is strictly newer even when local modification occurred
/// at the upper boundary of a 1-second interval.
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
    let client = Client::init(username, password, url)?;
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

/// Outcome of a concurrent upload batch.
enum UploadOutcome {
    /// All queued files were processed; carries the count successfully copied.
    Completed(usize),
    /// The user canceled mid-batch; copied-so-far is reflected in the log output.
    Canceled,
}

/// Blocks a worker until the live limit has room, releasing the slot on drop.
/// Mirrors the scan pool's dispatch gate so changing "Threads" affects uploads too.
struct WorkerGate {
    active: Arc<Mutex<usize>>,
    cvar: Arc<Condvar>,
    /// Kept alive so the global wakeup registry can reach this gate.
    _wake: Arc<PoolWake>,
}

struct WorkerSlot<'a> {
    gate: &'a WorkerGate,
}

impl WorkerGate {
    fn new() -> Self {
        let active = Arc::new(Mutex::new(0));
        let cvar = Arc::new(Condvar::new());
        let wake: Arc<PoolWake> = {
            let active = Arc::clone(&active);
            let cvar = Arc::clone(&cvar);
            Arc::new(move || {
                // Hold the gate lock while notifying so a worker cannot miss the
                // wakeup between checking the limit and parking on the condvar.
                let _held = active.lock().unwrap_or_else(|p| p.into_inner());
                cvar.notify_all();
            })
        };
        register_concurrency_wakeup(&wake);
        Self {
            active,
            cvar,
            _wake: wake,
        }
    }

    fn acquire(&self, stop: &AtomicBool, cancel: &AtomicBool) -> Option<WorkerSlot<'_>> {
        let mut active = self.active.lock().unwrap_or_else(|p| p.into_inner());
        loop {
            if stop.load(Ordering::SeqCst) || cancel.load(Ordering::SeqCst) {
                return None;
            }
            if *active < current_scan_concurrency() {
                *active += 1;
                return Some(WorkerSlot { gate: self });
            }
            active = self.cvar.wait(active).unwrap_or_else(|p| p.into_inner());
        }
    }

    /// Wakes parked workers so they can observe a stop flag set by a panic.
    fn wake(&self) {
        let _held = self.active.lock().unwrap_or_else(|p| p.into_inner());
        self.cvar.notify_all();
    }
}

impl Drop for WorkerSlot<'_> {
    fn drop(&mut self) {
        let mut active = self.gate.active.lock().unwrap_or_else(|p| p.into_inner());
        *active = active.saturating_sub(1);
        self.gate.cvar.notify_all();
    }
}

/// Uploads a batch of local files to the remote URL concurrently.
///
/// `concurrency` seeds the live limit (clamped to 1..=MAX_SCAN_CONCURRENCY) when
/// `Some`; `None` keeps it. The scan and upload pools share that limit, and it is
/// re-read for every claimed file, so a Settings change applies to an upload in flight.
///
/// Design mirrors `list_remote_recursive_with_options_and_log`:
/// - Work is distributed via a shared atomic cursor over `files`.
/// - Per-file log lines are funneled through an mpsc channel because the `log`
///   callback is `FnMut` (not `Sync`).
/// - `Client` is `Send + Sync` (asserted at compile time above),
///   so workers share one connection pool.
/// - Remote-cache updates are serialized behind a mutex because
///   `update_remote_cache_item` performs a read-modify-write of the cache file.
///
/// Error semantics match the former sequential loop: per-file failures (oversize,
/// MKCOL, open, non-2xx status, and transport failures that survive every retry)
/// are logged and skipped, while a worker panic still aborts the batch with `Err`.
fn upload_files_concurrently<F>(
    client: &Client,
    remote_url: &str,
    files: Vec<(PathBuf, String, u64)>,
    cancel_flag: &AtomicBool,
    concurrency: Option<usize>,
    auth_user: Option<&str>,
    max_size: u64,
    with_checksum: bool,
    done_label: &str,
    log: &mut F,
) -> Result<UploadOutcome, String>
where
    F: FnMut(&str),
{
    let total = files.len();
    if total == 0 {
        return Ok(UploadOutcome::Completed(0));
    }
    if let Some(requested) = concurrency {
        set_scan_concurrency(requested);
    }
    // Grow the pool from the current limit as Settings raises it, instead of
    // pre-allocating the full 64-thread ceiling up front.
    let gate = WorkerGate::new();

    let next_idx = AtomicUsize::new(0);
    let copied = AtomicUsize::new(0);
    let stop = AtomicBool::new(false);
    let error: Mutex<Option<String>> = Mutex::new(None);
    let cache_write_lock = Mutex::new(());

    /// Live worker count plus the condvar that wakes the spawn controller.
    struct UploadControl {
        live_workers: usize,
    }

    let control = Arc::new((
        Mutex::new(UploadControl { live_workers: 0 }),
        Condvar::new(),
    ));
    let control_wake: Arc<PoolWake> = {
        let control = Arc::clone(&control);
        Arc::new(move || {
            // Hold the control lock so the controller cannot miss this wakeup
            // between checking live_workers and parking on the condvar.
            let _held = control.0.lock().unwrap_or_else(|p| p.into_inner());
            control.1.notify_all();
        })
    };
    register_concurrency_wakeup(&control_wake);
    let _control_wake = control_wake;

    struct LiveWorkerGuard {
        control: Arc<(Mutex<UploadControl>, Condvar)>,
    }

    impl Drop for LiveWorkerGuard {
        fn drop(&mut self) {
            let mut control = self.control.0.lock().unwrap_or_else(|p| p.into_inner());
            control.live_workers = control.live_workers.saturating_sub(1);
            self.control.1.notify_all();
        }
    }

    /// Per-batch state shared by every upload worker.
    #[derive(Clone, Copy)]
    struct UploadCtx<'a> {
        client: &'a Client,
        remote_url: &'a str,
        files: &'a [(PathBuf, String, u64)],
        next_idx: &'a AtomicUsize,
        copied: &'a AtomicUsize,
        stop: &'a AtomicBool,
        error: &'a Mutex<Option<String>>,
        cache_write_lock: &'a Mutex<()>,
        cancel_flag: &'a AtomicBool,
        auth_user: Option<&'a str>,
        gate: &'a WorkerGate,
        total: usize,
        max_size: u64,
        with_checksum: bool,
        done_label: &'a str,
    }

    let ctx = UploadCtx {
        client,
        remote_url,
        files: &files,
        next_idx: &next_idx,
        copied: &copied,
        stop: &stop,
        error: &error,
        cache_write_lock: &cache_write_lock,
        cancel_flag,
        auth_user,
        gate: &gate,
        total,
        max_size,
        with_checksum,
        done_label,
    };
    let ctx_ref = &ctx;
    let control_arc = Arc::clone(&control);

    fn spawn_upload_worker<'scope, 'env: 'scope>(
        scope: &'scope std::thread::Scope<'scope, 'env>,
        ctx: &'env UploadCtx<'env>,
        control: Arc<(Mutex<UploadControl>, Condvar)>,
        log_tx: mpsc::Sender<String>,
    ) {
        let worker_log_tx = log_tx;
        scope.spawn(move || {
        let _live = LiveWorkerGuard { control };
        let UploadCtx {
            client,
            remote_url,
            files: files_ref,
            next_idx: next_idx_ref,
            copied: copied_ref,
            stop: stop_ref,
            error: error_ref,
            cache_write_lock: cache_write_lock_ref,
            cancel_flag,
            auth_user,
            gate: gate_ref,
            total,
            max_size,
            with_checksum,
            done_label,
        } = *ctx;
            let run_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                loop {
                    let Some(_slot) = gate_ref.acquire(stop_ref, cancel_flag) else {
                        return;
                    };
                    if stop_ref.load(Ordering::SeqCst)
                        || cancel_flag.load(Ordering::SeqCst)
                    {
                        return;
                    }
                    let idx = next_idx_ref.fetch_add(1, Ordering::SeqCst);
                    if idx >= total {
                        return;
                    }
                    let (path, rel_str, size) = &files_ref[idx];
                    let current_num = idx + 1;
                    if *size > max_size {
                        let _ = worker_log_tx.send(format!(
                            "[{}/{}] ERROR: File {} exceeds maximum size limit ({} bytes > {} bytes), skipping.\n",
                            current_num, total, rel_str, size, max_size
                        ));
                        continue;
                    }
                    let file_url = build_file_url(remote_url, rel_str);
                    let _ = worker_log_tx.send(format!(
                        "[{}/{}] Uploading: {} ({} bytes)...\n",
                        current_num, total, rel_str, size
                    ));
                    if let Err(e) = ensure_remote_parent_dirs(
                        client,
                        remote_url,
                        rel_str,
                        cancel_flag,
                        &mut |msg: &str| {
                            let _ = worker_log_tx.send(msg.to_string());
                        },
                    ) {
                        let _ = worker_log_tx.send(format!(
                            "[{}/{}] ERROR: Failed creating remote directory for {}: {}\n",
                            current_num, total, rel_str, e
                        ));
                        continue;
                    }
                    let checksum_str = if with_checksum {
                        match compute_file_sha256(path) {
                            Ok(hash) => format!(" (sha256: {})", hash),
                            Err(_) => String::new(),
                        }
                    } else {
                        String::new()
                    };
                    let put_result = retry_webdav_operation(
                        &format!("PUT {file_url}"),
                        cancel_flag,
                        REQUEST_MAX_ATTEMPTS,
                        RETRY_DELAY,
                        |msg: &str| {
                            let _ = worker_log_tx.send(msg.to_string());
                        },
                        || {
                            // PUT consumes the handle, so each attempt opens the file afresh.
                            let file = std::fs::File::open(path).map_err(|e| {
                                AttemptError::Fatal(format!(
                                    "Failed to open {}: {}",
                                    path.display(),
                                    e
                                ))
                            })?;
                            client.put(file, &file_url).map_err(|e| {
                                classify_dav_error(&format!("PUT {file_url}"), &e)
                            })
                        },
                    );
                    let res = match put_result {
                        Ok(r) => r,
                        Err(e) => {
                            if cancel_flag.load(Ordering::SeqCst) {
                                return;
                            }
                            // The file had every retry and still failed; skip it and keep
                            // uploading the rest of the batch.
                            let _ = worker_log_tx.send(format!(
                                "[{}/{}] ERROR: Upload failed for {}: {}\n",
                                current_num, total, rel_str, e
                            ));
                            continue;
                        }
                    };
                    if res.status().is_success() {
                        copied_ref.fetch_add(1, Ordering::SeqCst);
                        if is_cache_enabled() {
                            let mtime = std::fs::metadata(path)
                                .ok()
                                .and_then(|m| get_metadata_mtime(&m));
                            // Serialize the cache read-modify-write across workers.
                            let _guard = cache_write_lock_ref
                                .lock()
                                .unwrap_or_else(|p| p.into_inner());
                            update_remote_cache_item(
                                remote_url,
                                auth_user,
                                rel_str,
                                *size,
                                mtime,
                            );
                        }
                        let _ = worker_log_tx.send(format!(
                            "[{}/{}] {}: {} ({} bytes){}\n",
                            current_num, total, done_label, rel_str, size, checksum_str
                        ));
                    } else {
                        let _ = worker_log_tx.send(format!(
                            "[{}/{}] ERROR: Failed to copy {}: HTTP {}\n",
                            current_num, total, rel_str, res.status()
                        ));
                    }
                }
            }));
            if run_result.is_err() {
                let mut err_slot = error_ref.lock().unwrap_or_else(|p| p.into_inner());
                if err_slot.is_none() {
                    *err_slot = Some("Upload worker thread panicked unexpectedly.".to_string());
                }
                stop_ref.store(true, Ordering::SeqCst);
                gate_ref.wake();
            }
        });
    }

    let (log_tx, log_rx) = mpsc::channel::<String>();

    std::thread::scope(|s| {
        // One controller thread owns spawning so the pool starts at the current
        // limit and grows as Settings raises it.
        let stop_ref = &stop;
        let cancel_ref = cancel_flag;
        let next_ref = &next_idx;
        let controller = s.spawn(move || {
            let mut control = control_arc.0.lock().unwrap_or_else(|p| p.into_inner());
            loop {
                if stop_ref.load(Ordering::SeqCst) || cancel_ref.load(Ordering::SeqCst) {
                    break;
                }
                let target = current_scan_concurrency().min(total);
                if control.live_workers < target && next_ref.load(Ordering::SeqCst) < total {
                    control.live_workers += 1;
                    drop(control);
                    spawn_upload_worker(s, ctx_ref, Arc::clone(&control_arc), log_tx.clone());
                    control = control_arc.0.lock().unwrap_or_else(|p| p.into_inner());
                    continue;
                }
                if control.live_workers == 0 {
                    break;
                }
                control = control_arc.1.wait(control).unwrap_or_else(|p| p.into_inner());
            }
        });

        while let Ok(msg) = log_rx.recv() {
            log(&msg);
        }
        let _ = controller.join();
    });

    let copied = copied.load(Ordering::SeqCst);
    let error = error.into_inner().unwrap_or_else(|p| p.into_inner());
    if let Some(err) = error {
        return Err(err);
    }
    if cancel_flag.load(Ordering::SeqCst) {
        return Ok(UploadOutcome::Canceled);
    }
    Ok(UploadOutcome::Completed(copied))
}

/// Executes a native WebDAV action and streams logs through the callback.
pub fn execute_webdav_action<F>(
    client: &Client,
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

/// Executes a native WebDAV action with custom options (such as worker concurrency and auth context) and streams logs.
///
/// `concurrency` seeds the live limit shared by the remote scan and upload worker
/// pools (put/sync). The limit is re-read while they run, so a Settings "Threads"
/// change applies to the phase that is already in progress.
pub fn execute_webdav_action_with_options<F>(
    client: &Client,
    action: &str,
    remote_url: &str,
    local_dir: &Path,
    cancel_flag: &AtomicBool,
    concurrency: Option<usize>,
    auth_user: Option<&str>,
    log: F,
) -> Result<(), String>
where
    F: FnMut(&str),
{
    execute_webdav_action_with_exclusions(
        client,
        action,
        remote_url,
        local_dir,
        cancel_flag,
        concurrency,
        auth_user,
        &[],
        log,
    )
}

/// Executes a native WebDAV action, pruning `exclude_folders` from every scan.
///
/// Excluded folders are skipped on both sides of an operation: the local walk never
/// descends into them and the remote traversal never requests them. As a result they
/// are never uploaded, downloaded, compared, or deleted by a mirror (`sync`).
pub fn execute_webdav_action_with_exclusions<F>(
    client: &Client,
    action: &str,
    remote_url: &str,
    local_dir: &Path,
    cancel_flag: &AtomicBool,
    concurrency: Option<usize>,
    auth_user: Option<&str>,
    exclude_folders: &[String],
    mut log: F,
) -> Result<(), String>
where
    F: FnMut(&str),
{
    validate_webdav_url(remote_url)?;
    // Seed the live limit once per action. Passing `None` to the phase helpers
    // below keeps a Settings change made mid-action from being overwritten.
    if let Some(requested) = concurrency {
        set_scan_concurrency(requested);
    }
    if !exclude_folders.is_empty() {
        log(&format!(
            "Excluding folder(s): {}\n",
            exclude_folders.join(", ")
        ));
    }
    match action {
        "ls" => {
            log(&format!(
                "Listing remote files and folders in {}...\n",
                remote_url
            ));
            let items = list_remote_recursive_with_exclusions_and_log(
                client,
                remote_url,
                cancel_flag,
                None,
                auth_user,
                true,
                exclude_folders,
                &mut log,
            )?;
            let mut file_count = 0;
            let mut dir_count = 0;
            let mut total_size = 0;
            for item in &items {
                let rel = relative_item_path(remote_url, &item.href);
                if rel.is_empty() {
                    continue;
                }
                if item.is_dir {
                    log(&format!("{:>9} {}/\n", -1, rel));
                    dir_count += 1;
                } else {
                    log(&format!("{:>9} {}\n", item.size, rel));
                    file_count += 1;
                    total_size += item.size;
                }
            }
            log(&format!(
                "\nTotal files: {}, Total directories: {}, Total size: {} bytes\n",
                file_count, dir_count, total_size
            ));
            Ok(())
        }
        "lsd" => {
            log(&format!("Listing remote directories in {}...\n", remote_url));
            let res = retry_webdav_operation(
                &format!("PROPFIND Depth: 1 {remote_url}"),
                cancel_flag,
                REQUEST_MAX_ATTEMPTS,
                RETRY_DELAY,
                &mut log,
                || {
                    client.list(remote_url, "1").map_err(|e| {
                        classify_dav_error(&format!("PROPFIND Depth: 1 {remote_url}"), &e)
                    })
                },
            )?;
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
                if rel.is_empty() || is_excluded_path(&rel, item.is_dir, exclude_folders) {
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
            let local_files = collect_local_files_with_mtime_excluding(local_dir, exclude_folders);
            if local_files.is_empty() {
                log("No local files found to copy.\n");
                return Ok(());
            }

            log(&format!(
                "Found {} local file(s). Querying remote files in {} to detect changes...\n",
                local_files.len(),
                remote_url
            ));
            let remote_items = list_remote_recursive_with_exclusions_and_log(
                client,
                remote_url,
                cancel_flag,
                None,
                auth_user,
                true,
                exclude_folders,
                &mut log,
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

            let max_size = get_max_file_size();
            if is_dry {
                // Dry-run performs no network I/O; keep it sequential for stable output order.
                for (idx, (_, rel_str, size)) in files_to_upload.iter().enumerate() {
                    if cancel_flag.load(Ordering::SeqCst) {
                        log("Operation canceled by user.\n");
                        return Ok(());
                    }
                    let current_num = idx + 1;
                    if *size > max_size {
                        log(&format!(
                            "[{}/{}] ERROR: File {} exceeds maximum size limit ({} bytes > {} bytes), skipping.\n",
                            current_num, total_to_upload, rel_str, size, max_size
                        ));
                        continue;
                    }
                    log(&format!(
                        "[{}/{}] NOTICE: {}: Would copy (new or modified, {} bytes)\n",
                        current_num, total_to_upload, rel_str, size
                    ));
                }
                log(&format!(
                    "\nPut operation finished: {} file(s) copied, {} file(s) skipped (already up to date).\n",
                    0, skipped_count
                ));
                return Ok(());
            }
            let copied_count = match upload_files_concurrently(
                client,
                remote_url,
                files_to_upload,
                cancel_flag,
                None,
                auth_user,
                max_size,
                with_checksum,
                "Copied",
                &mut log,
            )? {
                UploadOutcome::Completed(n) => n,
                UploadOutcome::Canceled => {
                    log("Operation canceled by user.\n");
                    return Ok(());
                }
            };
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
            let items = list_remote_recursive_with_exclusions_and_log(
                client,
                remote_url,
                cancel_flag,
                None,
                auth_user,
                false,
                exclude_folders,
                &mut log,
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
            let local_files = collect_local_files_with_mtime_excluding(local_dir, exclude_folders);
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
                // A server-supplied href must not redirect an authenticated
                // GET to another host: require the same origin as the
                // configured remote and pass the SSRF guard. Checked before
                // any filesystem touch — a skipped item creates nothing.
                let download_url = resolve_item_url(remote_url, &item.href);
                if let Err(reason) = guard_server_derived_url(remote_url, &download_url) {
                    log(&format!(
                        "[{}/{}] ERROR: Skipping {}: {}\n",
                        current_num, total_files, rel, reason
                    ));
                    continue;
                }
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
                let download_result = retry_webdav_operation(
                    &format!("GET {download_url}"),
                    cancel_flag,
                    REQUEST_MAX_ATTEMPTS,
                    RETRY_DELAY,
                    &mut log,
                    || {
                        let get_res = client.get(&download_url).map_err(|e| {
                            classify_dav_error(&format!("GET {download_url}"), &e)
                        })?;
                        let status = get_res.status();
                        if !status.is_success() {
                            return Err(AttemptError::Fatal(format!(
                                "GET {download_url} returned HTTP {status}"
                            )));
                        }
                        // Recreate the file on every attempt so a retried download never appends
                        // to bytes left behind by a timed-out body stream.
                        let mut dest_file = std::fs::File::create(&target_file).map_err(|e| {
                            AttemptError::Fatal(format!(
                                "Failed to create {}: {}",
                                target_file.display(),
                                e
                            ))
                        })?;
                        let mut limited_reader = std::io::Read::take(get_res, max_size + 1);
                        // Manual copy loop: io::copy neither observes the
                        // cancel flag nor any whole-body deadline, and each
                        // blocking read gets a fresh 30s window from reqwest,
                        // so a slow-drip body would hang the attempt forever.
                        // One read parked inside reqwest can still delay the
                        // checks below by at most that per-read timeout; the
                        // deadline bounds every other stretch of the transfer.
                        let body_deadline = std::time::Instant::now()
                            + std::time::Duration::from_secs(get_download_body_timeout_secs());
                        let mut buffer = [0u8; 65536];
                        let mut written: u64 = 0;
                        loop {
                            if cancel_flag.load(Ordering::SeqCst) {
                                // The outer error path checks the flag again,
                                // cleans the partial, and ends the operation
                                // as a user cancellation.
                                return Err(AttemptError::Fatal(
                                    "Operation canceled by user.".to_string(),
                                ));
                            }
                            if std::time::Instant::now() >= body_deadline {
                                return Err(AttemptError::Fatal(format!(
                                    "Download body exceeded the {}s time limit",
                                    get_download_body_timeout_secs()
                                )));
                            }
                            use std::io::Read as _;
                            match limited_reader.read(&mut buffer) {
                                Ok(0) => break,
                                Ok(n) => {
                                    dest_file
                                        .write_all(&buffer[..n])
                                        .map_err(|e| {
                                            classify_io_error(
                                                &format!("writing GET {download_url}"),
                                                &e,
                                            )
                                        })?;
                                    written += n as u64;
                                    if written > max_size {
                                        break;
                                    }
                                }
                                // io::copy retried EINTR reads; keep that
                                // contract so a signal does not fail an
                                // otherwise healthy transfer.
                                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                                Err(e) => {
                                    return Err(classify_io_error(
                                        &format!("streaming GET {download_url}"),
                                        &e,
                                    ))
                                }
                            }
                        }
                        use std::io::Write as _;
                        if written > max_size {
                            drop(dest_file);
                            let _ = std::fs::remove_file(&target_file);
                            return Err(AttemptError::Fatal(format!(
                                "Downloaded file exceeded the maximum size limit ({} bytes > {} bytes)",
                                written, max_size
                            )));
                        }
                        Ok(written)
                    },
                );
                let downloaded_len = match download_result {
                    Ok(len) => len,
                    Err(e) => {
                        // Never leave a truncated partial behind after the final attempt.
                        if let Err(remove_err) = std::fs::remove_file(&target_file) {
                            if remove_err.kind() != std::io::ErrorKind::NotFound {
                                log(&format!(
                                    "[{}/{}] WARNING: Could not remove partial download {}: {}\n",
                                    current_num,
                                    total_files,
                                    target_file.display(),
                                    remove_err
                                ));
                            }
                        }
                        if cancel_flag.load(Ordering::SeqCst) {
                            log("Operation canceled by user.\n");
                            return Ok(());
                        }
                        log(&format!(
                            "[{}/{}] ERROR: Download failed for {}: {}\n",
                            current_num, total_files, rel, e
                        ));
                        continue;
                    }
                };

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
            let remote_items = list_remote_recursive_with_exclusions_and_log(
                client,
                remote_url,
                cancel_flag,
                None,
                auth_user,
                true,
                exclude_folders,
                &mut log,
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
            let local_files = collect_local_files_excluding(local_dir, exclude_folders);
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
            let local_files = collect_local_files_with_mtime_excluding(local_dir, exclude_folders);
            let mut local_set: HashSet<String> = HashSet::new();

            log(&format!("Querying remote files in {} to detect changes...\n", remote_url));
            let remote_items = list_remote_recursive_with_exclusions_and_log(
                client,
                remote_url,
                cancel_flag,
                None,
                auth_user,
                true,
                exclude_folders,
                &mut log,
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

            let max_size = get_max_file_size();
            let uploaded = match upload_files_concurrently(
                client,
                remote_url,
                files_to_upload,
                cancel_flag,
                None,
                auth_user,
                max_size,
                false,
                "Synced",
                &mut log,
            )? {
                UploadOutcome::Completed(n) => n,
                UploadOutcome::Canceled => {
                    log("Operation canceled by user.\n");
                    return Ok(());
                }
            };
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
                    // A server-supplied href must not redirect the
                    // authenticated DELETE to another host.
                    if let Err(reason) = guard_server_derived_url(remote_url, &file_url) {
                        log(&format!(
                            "ERROR: Skipping delete of {}: {}\n",
                            rel, reason
                        ));
                        continue;
                    }
                    let del_res = retry_webdav_operation(
                        &format!("DELETE {file_url}"),
                        cancel_flag,
                        REQUEST_MAX_ATTEMPTS,
                        RETRY_DELAY,
                        &mut log,
                        || {
                            client.delete(&file_url).map_err(|e| {
                                classify_dav_error(&format!("DELETE {file_url}"), &e)
                            })
                        },
                    );
                    match del_res {
                        Ok(_) => {
                            if is_cache_enabled() {
                                remove_remote_cache_item(remote_url, auth_user, &rel);
                            }
                            log(&format!("Deleted remote file not in local: {}\n", rel));
                            deleted += 1;
                        }
                        Err(e) => {
                            if cancel_flag.load(Ordering::SeqCst) {
                                log("Operation canceled by user.\n");
                                return Ok(());
                            }
                            log(&format!(
                                "ERROR: Failed to delete remote file {}: {}\n",
                                rel, e
                            ));
                        }
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
    fn test_retry_delay_grows_and_caps() {
        let base = Duration::from_millis(100);
        assert_eq!(retry_delay(1, base), Duration::from_millis(100));
        assert_eq!(retry_delay(2, base), Duration::from_millis(200));
        assert_eq!(retry_delay(3, base), Duration::from_millis(300));
        assert_eq!(retry_delay(0, base), Duration::from_millis(100));
        assert_eq!(
            retry_delay(10_000, base),
            Duration::from_millis(RETRY_MAX_DELAY_MS)
        );
    }

    #[test]
    fn test_retry_operation_recovers_from_transient_failures() {
        let cancel = AtomicBool::new(false);
        let attempts = std::cell::Cell::new(0u32);
        let mut logs = String::new();
        let result = retry_webdav_operation(
            "PUT https://example.test/file.txt",
            &cancel,
            3,
            Duration::ZERO,
            |msg: &str| logs.push_str(msg),
            || {
                attempts.set(attempts.get() + 1);
                if attempts.get() < 3 {
                    Err(AttemptError::Transient("operation timed out".to_string()))
                } else {
                    Ok("uploaded")
                }
            },
        );
        assert_eq!(result, Ok("uploaded"));
        assert_eq!(attempts.get(), 3);
        assert!(logs.contains("attempt 1/3 hit a transient transport failure"));
        assert!(logs.contains("recovered on attempt 3/3"));
    }

    #[test]
    fn test_retry_operation_does_not_retry_permanent_failures() {
        let cancel = AtomicBool::new(false);
        let attempts = std::cell::Cell::new(0u32);
        let result: Result<(), String> = retry_webdav_operation(
            "MKCOL https://example.test/dir",
            &cancel,
            3,
            Duration::ZERO,
            |_| {},
            || {
                attempts.set(attempts.get() + 1);
                Err(AttemptError::Fatal("HTTP 403 Forbidden".to_string()))
            },
        );
        assert!(result.is_err());
        assert_eq!(attempts.get(), 1);
    }

    #[test]
    fn test_retry_operation_gives_up_after_max_attempts() {
        let cancel = AtomicBool::new(false);
        let attempts = std::cell::Cell::new(0u32);
        let mut logs = String::new();
        let result: Result<(), String> = retry_webdav_operation(
            "GET https://example.test/file.bin",
            &cancel,
            2,
            Duration::ZERO,
            |msg: &str| logs.push_str(msg),
            || {
                attempts.set(attempts.get() + 1);
                Err(AttemptError::Transient("connection reset".to_string()))
            },
        );
        assert!(result.is_err());
        assert_eq!(attempts.get(), 2);
        assert!(logs.contains("giving up after 2 attempt(s)"));
    }

    #[test]
    fn test_retry_operation_honors_cancellation_before_first_attempt() {
        let cancel = AtomicBool::new(true);
        let attempts = std::cell::Cell::new(0u32);
        let result: Result<(), String> = retry_webdav_operation(
            "GET https://example.test/file.bin",
            &cancel,
            5,
            Duration::ZERO,
            |_| {},
            || {
                attempts.set(attempts.get() + 1);
                Ok(())
            },
        );
        assert_eq!(result, Err("Operation canceled by user.".to_string()));
        assert_eq!(attempts.get(), 0);
    }

    #[test]
    fn test_classify_io_error_retries_network_kinds_only() {
        let timed_out = std::io::Error::new(std::io::ErrorKind::TimedOut, "socket timed out");
        assert!(matches!(
            classify_io_error("streaming GET x", &timed_out),
            AttemptError::Transient(_)
        ));
        let truncated = std::io::Error::new(std::io::ErrorKind::UnexpectedEof, "body truncated");
        assert!(matches!(
            classify_io_error("streaming GET x", &truncated),
            AttemptError::Transient(_)
        ));
        let disk_full = std::io::Error::new(std::io::ErrorKind::Other, "No space left on device");
        assert!(matches!(
            classify_io_error("streaming GET x", &disk_full),
            AttemptError::Fatal(_)
        ));
    }

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
        assert!(reqwest::Url::parse(&url).is_ok());
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
    fn test_relative_item_path_multibyte_char_boundary_is_panic_free() {
        // decode_percent turns server hrefs into multi-byte UTF-8; splitting
        // the item path at the byte length of the base path must never land
        // inside a character (panic -> whole operation aborts via JoinError).
        let base = "https://h.example/dav/";
        // '/aaéx' decodes from '/aa%C3%A9x': byte index 4 of the item path
        // lands inside the 2-byte 'é' when the base path is '/dav' (len 4).
        let rel = relative_item_path(base, "/aa%C3%A9x");
        assert!(rel.is_empty() || is_safe_relative_path(&rel) || !is_safe_relative_path(&rel));
        // No panic is the invariant; exercise a sweep of lengths.
        for n in 1..12 {
            let item = format!("/{}%C3%A9x", "a".repeat(n));
            let _ = relative_item_path(base, &item);
        }
    }

    #[test]
    fn test_validate_webdav_url_ipv6_literals_and_mapped_ranges() {
        // Bracketed IPv6 literals reach the IP check (host_str returns them
        // bracketed, which the old bare host.parse() never matched).
        assert!(validate_webdav_url_internal("https://[::1]/dav", false).is_err());
        assert!(validate_webdav_url_internal("https://[fe80::1]/dav", false).is_err());
        assert!(validate_webdav_url_internal("https://[fd00::1]/dav", false).is_err());
        // IPv4-mapped IPv6 spellings of blocked ranges must not pass.
        assert!(validate_webdav_url_internal("https://[::ffff:127.0.0.1]/dav", false).is_err());
        assert!(validate_webdav_url_internal("https://[::ffff:10.0.0.1]/dav", false).is_err());
        assert!(validate_webdav_url_internal(
            "https://[::ffff:169.254.169.254]/latest/meta-data",
            false
        )
        .is_err());
        // NAT64 well-known prefix embedding a blocked IPv4.
        assert!(validate_webdav_url_internal("https://[64:ff9b::a00:1]/dav", false).is_err());
        // A public IPv6 literal is still accepted.
        assert!(validate_webdav_url_internal("https://[2606:4700::6810:85e5]/dav", false).is_ok());
    }

    #[test]
    fn test_validate_webdav_url_embedded_ipv4_spellings() {
        // NAT64 64:ff9b::/96 embeds the IPv4 in the last 32 bits, so the public
        // form must be accepted and only genuinely blocked targets rejected.
        assert!(validate_webdav_url_internal("https://[64:ff9b::808:808]/dav", false).is_ok());
        assert!(validate_webdav_url_internal("https://[64:ff9b::a00:1]/dav", false).is_err());
        assert!(validate_webdav_url_internal("https://[64:ff9b::7f00:1]/dav", false).is_err());
        // 6to4 carries the IPv4 in segments 1-2 of the 2002::/16 prefix.
        assert!(validate_webdav_url_internal("https://[2002:a00:1::]/dav", false).is_err());
        assert!(validate_webdav_url_internal("https://[2002:808:808::]/dav", false).is_ok());
        // Teredo carries a server IPv4 and a bit-inverted client IPv4.
        assert!(validate_webdav_url_internal("https://[2001:0:a00:1::]/dav", false).is_err());
        // Public Teredo server, client 10.0.0.1 inverted into the last 32 bits.
        assert!(validate_webdav_url_internal(
            "https://[2001:0:808:808:0:0:f5ff:fffe]/dav",
            false
        )
        .is_err());
        // Local-use NAT64 (RFC 8215) is not a public destination.
        assert!(validate_webdav_url_internal("https://[64:ff9b:1::808:808]/dav", false).is_err());
        // IPv4-compatible ::a.b.c.d spellings stay covered alongside mapped ones.
        assert!(validate_webdav_url_internal("https://[::a00:1]/dav", false).is_err());
    }

    #[test]
    fn test_validate_webdav_url_fails_closed_when_host_does_not_resolve() {
        // An unanswered lookup must not be read as "no blocked address found":
        // a host that fails validation-time resolution can resolve to a private
        // address when the request is actually issued.
        let result = validate_webdav_url_internal("https://does-not-resolve.invalid/dav", false);
        assert!(result.is_err(), "unresolvable host must be rejected");
        assert!(result.unwrap_err().contains("Could not resolve"));
    }

    #[test]
    fn test_same_remote_origin_rejects_crosshost_hrefs() {
        let base = "https://example.com/remote.php/webdav/folder/";
        // Same-origin absolute hrefs pass.
        assert!(same_remote_origin(
            base,
            "https://example.com/remote.php/webdav/folder/file.txt"
        ));
        assert!(same_remote_origin(base, "https://example.com/other/path"));
        // Cross-host hrefs are rejected — the SSRF/credential-leak guard.
        assert!(!same_remote_origin(base, "https://attacker.example/harvest"));
        assert!(!same_remote_origin(
            base,
            "http://169.254.169.254/latest/meta-data"
        ));
        assert!(!same_remote_origin(base, "http://localhost:9000/bucket/obj"));
        // Scheme change is a different origin.
        assert!(!same_remote_origin(
            base,
            "http://example.com/remote.php/webdav/folder/file.txt"
        ));
    }

    #[test]
    fn test_url_userinfo_credentials_rejected() {
        // A password embedded in the URL is persisted to settings.json and
        // logged verbatim — it must be rejected before any storage or request.
        assert!(validate_webdav_url_internal(
            "https://user:secretpass@example.com/dav/",
            false
        )
        .is_err());
        assert!(validate_webdav_url_internal(
            "https://user:secretpass@example.com/dav/",
            true
        )
        .is_err());
        // Username-only userinfo is rejected too: it still bypasses the vault.
        assert!(validate_webdav_url_internal(
            "https://user@example.com/dav/",
            false
        )
        .is_err());
        // The same host without embedded credentials is fine.
        assert!(validate_webdav_url_internal("https://example.com/dav/", false).is_ok());
    }

    #[test]
    fn test_http_scheme_rejected_in_production_sniffer_cannot_read_credentials() {
        // A passive sniffer on the local network reads every plain-text byte.
        // Production mode must refuse any scheme it could read, so the
        // Basic-auth password can never travel unencrypted.
        for url in [
            "http://example.com/",
            "http://example.com/dav/",
            "http://10.0.0.1/", // would also fail private-range, scheme first
        ] {
            assert!(
                validate_webdav_url_internal(url, false).is_err(),
                "production mode must reject sniffer-readable scheme: {url}"
            );
        }
    }

    #[test]
    fn test_cache_and_settings_never_store_password() {
        // The listing cache must never carry the credential: serialize an
        // entry with a marker "password" and prove the struct has no field
        // that would emit it.
        let entry = CachedRemoteListing {
            remote_url: "https://example.com/dav/".to_string(),
            auth_user: Some("user".to_string()),
            timestamp: 0,
            items: vec![WebdavItem::new("dav/file.txt", false, 3)],
        };
        let json = serde_json::to_string(&entry).unwrap();
        assert!(
            !json.to_ascii_lowercase().contains("password"),
            "cache serialization leaked a password field: {json}"
        );
        assert!(!json.contains("secretpass"));
        // auth_user carries only the username, never the secret.
        assert!(json.contains("user"));
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
    fn test_set_scan_concurrency_updates_live_value_and_gates_workers() {
        set_scan_concurrency(MAX_SCAN_CONCURRENCY + 100);
        assert_eq!(current_scan_concurrency(), MAX_SCAN_CONCURRENCY);
        set_scan_concurrency(0);
        assert_eq!(current_scan_concurrency(), 1);
        set_scan_concurrency(3);
        assert_eq!(current_scan_concurrency(), 3);

        // A gated worker must stay parked at the limit and be admitted when it rises.
        set_scan_concurrency(1);
        let gate = WorkerGate::new();
        let stop = AtomicBool::new(false);
        let cancel = AtomicBool::new(false);
        let held = gate.acquire(&stop, &cancel).expect("first slot");
        let admitted = AtomicBool::new(false);
        std::thread::scope(|scope| {
            scope.spawn(|| {
                let _slot = gate.acquire(&stop, &cancel);
                admitted.store(true, Ordering::SeqCst);
            });
            std::thread::sleep(Duration::from_millis(100));
            assert!(!admitted.load(Ordering::SeqCst), "worker exceeded the live limit");
            set_scan_concurrency(2);
        });
        assert!(admitted.load(Ordering::SeqCst), "worker was not admitted after the limit was raised");
        drop(held);

        // Cancellation must release parked workers even when no slot is free.
        cancel.store(true, Ordering::SeqCst);
        assert!(gate.acquire(&stop, &cancel).is_none(), "cancel must win over the wait");

        set_scan_concurrency(DEFAULT_SCAN_CONCURRENCY);
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

    #[test]
    fn test_normalize_exclude_folders() {
        let raw = vec![
            " .git/ ".to_string(),
            "/node_modules/".to_string(),
            "assets\\cache".to_string(),
            "assets//cache".to_string(),
            ".".to_string(),
            "..".to_string(),
            "a/../b".to_string(),
            "".to_string(),
            "build/".to_string(),
        ];
        assert_eq!(
            normalize_exclude_folders(&raw),
            vec![
                ".git".to_string(),
                "node_modules".to_string(),
                "assets/cache".to_string(),
                "build".to_string(),
            ]
        );
        assert_eq!(
            normalize_exclude_pattern("  ./.git/  "),
            Some(".git".to_string())
        );
        assert_eq!(normalize_exclude_pattern(".."), None);
        assert_eq!(normalize_exclude_pattern("a/../b"), None);
        assert_eq!(normalize_exclude_pattern("bad\nname"), None);
        assert_eq!(normalize_exclude_pattern("bad\tname"), None);
        assert_eq!(normalize_exclude_pattern("/"), None);
        assert_eq!(
            normalize_exclude_pattern(&"x".repeat(MAX_EXCLUDE_PATTERN_CHARS + 1)),
            None
        );
    }

    #[test]
    fn test_is_excluded_path_matches_names_and_relative_paths() {
        let patterns = normalize_exclude_folders(&[
            ".git".to_string(),
            "node_modules".to_string(),
            "assets/cache".to_string(),
        ]);

        // Name-only patterns match a folder at any depth and everything inside it.
        assert!(is_excluded_path(".git", true, &patterns));
        assert!(is_excluded_path(".git/config", false, &patterns));
        assert!(is_excluded_path("sub/.git/HEAD", false, &patterns));
        assert!(is_excluded_path("sub/.git", true, &patterns));
        assert!(is_excluded_path("node_modules/pkg/index.js", false, &patterns));

        // Path patterns match only that relative folder and its descendants.
        assert!(is_excluded_path("assets/cache", true, &patterns));
        assert!(is_excluded_path("assets/cache/data.bin", false, &patterns));

        // A *file* whose name matches a folder pattern is not an excluded folder.
        assert!(!is_excluded_path(".git", false, &patterns));
        assert!(!is_excluded_path("sub/.git", false, &patterns));
        assert!(!is_excluded_path("assets/cache", false, &patterns));

        // Similar names and prefixes must not be excluded.
        assert!(!is_excluded_path("assets/cache2/data.bin", false, &patterns));
        assert!(!is_excluded_path(".gitlab/config", false, &patterns));
        assert!(!is_excluded_path("assets/keep.txt", false, &patterns));
        assert!(!is_excluded_path("", true, &patterns));
        assert!(!is_excluded_path(".git", true, &[]));
    }

    #[test]
    fn test_collect_local_files_prunes_excluded_folders() {
        let temp_dir = std::env::temp_dir().join(format!(
            "scs_exclude_test_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = std::fs::remove_dir_all(&temp_dir);
        let write = |rel: &str| {
            let path = temp_dir.join(rel);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, b"data").unwrap();
        };
        write("keep.txt");
        write("assets/keep.txt");
        write("assets/cache/data.bin");
        write("assets/cache2/keep.txt");
        write(".git/config");
        write("sub/.git/HEAD");
        write("node_modules/pkg/index.js");
        // A regular file named like a folder pattern must still be collected.
        write("logs");
        write("nested/logs/keep.txt");

        let patterns = normalize_exclude_folders(&[
            ".git/".to_string(),
            "node_modules".to_string(),
            "assets/cache".to_string(),
            "logs".to_string(),
        ]);
        let mut collected: Vec<String> = collect_local_files_excluding(&temp_dir, &patterns)
            .into_iter()
            .map(|(_, rel, _)| rel)
            .collect();
        collected.sort();
        assert_eq!(
            collected,
            vec![
                "assets/cache2/keep.txt".to_string(),
                "assets/keep.txt".to_string(),
                "keep.txt".to_string(),
                "logs".to_string(),
            ]
        );

        let _ = std::fs::remove_dir_all(&temp_dir);
    }

    #[test]
    fn test_download_body_timeout_clamps_instead_of_overflowing() {
        // A huge configured deadline must clamp to a sane maximum
        // (DOWNLOAD_BODY_TIMEOUT_MAX_SECS, one day) instead of reaching the
        // copy loop, where `Instant::now() + Duration::from_secs(u64::MAX)`
        // would panic on overflow.
        std::env::set_var("WEBDAV_DOWNLOAD_BODY_TIMEOUT_SECS", "18446744073709551615");
        let configured = get_download_body_timeout_secs();
        std::env::remove_var("WEBDAV_DOWNLOAD_BODY_TIMEOUT_SECS");
        assert!(
            configured <= 86_400,
            "configured deadline must clamp to one day, got {configured}"
        );
        // The clamped ceiling must stay panic-free in deadline arithmetic.
        let deadline =
            std::time::Instant::now() + std::time::Duration::from_secs(configured.max(1));
        assert!(deadline > std::time::Instant::now());
    }
}

