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
/// Hard byte cap for a single server-controlled listing body (PROPFIND
/// multistatus). GET downloads are capped by `DEFAULT_MAX_FILE_SIZE`; listing
/// bodies must be bounded the same way or one response can exhaust memory.
pub const MAX_LISTING_BODY_BYTES: u64 = 64 * 1024 * 1024;
/// Ceiling on the total bytes of listing bodies buffered concurrently across
/// scan workers. The per-body cap alone would let `MAX_SCAN_CONCURRENCY`
/// workers each hold a full `MAX_LISTING_BODY_BYTES` body (~4 GiB) before any
/// aggregate check runs, so every listing read reserves its worst case
/// against this shared budget and fails closed beyond it.
pub const MAX_INFLIGHT_LISTING_BYTES: usize = 512 * 1024 * 1024;
/// Aggregate item cap across one scan: bounds memory, the cache file, and the
/// number of local files a hostile listing can materialize via `get`. The
/// same bound is enforced per listing during XML item collection, so no
/// single PROPFIND response can materialize unbounded item state either.
pub const MAX_REMOTE_ITEMS: usize = 100_000;
/// Hard cap on one server-advertised href: item counts alone do not bound
/// retained bytes, and a single href could otherwise carry most of a listing
/// body into per-item scan state past every aggregate budget.
pub const MAX_REMOTE_HREF_BYTES: usize = 4096;
/// Aggregate retained-bytes budget for one scan's item state (hrefs held in
/// `all_items`/`seen_items` and the listing cache), complementing the
/// per-listing in-flight budget with a whole-scan bound.
pub const MAX_RETAINED_LISTING_BYTES: usize = 256 * 1024 * 1024;
/// Per-entry overhead the retained budget charges for the Vec/set nodes
/// around each href copy.
const RETAINED_ITEM_OVERHEAD_BYTES: usize = 128;
/// Whole-scan budget on items PROCESSED (duplicates included): dedup means
/// duplicates never reach the retained budget, but the parse work they
/// force is bounded here instead.
pub const MAX_PROCESSED_LISTING_ITEMS: usize = 1_000_000;
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
/// Configured values are clamped to one day: anything larger serves no
/// plausible transfer and would overflow `Instant + Duration` into a panic.
pub const DOWNLOAD_BODY_TIMEOUT_MAX_SECS: u64 = 86_400;

pub fn get_download_body_timeout_secs() -> u64 {
    std::env::var("WEBDAV_DOWNLOAD_BODY_TIMEOUT_SECS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .filter(|v| *v > 0)
        .unwrap_or(600)
        .min(DOWNLOAD_BODY_TIMEOUT_MAX_SECS)
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

/// True when `subdir` contains traversal segments or other path-hostile
/// bytes, checking the raw text and each percent-decoding of it until the
/// text stabilizes: `..` segments, backslashes, and control characters count
/// in every form, because [`build_remote_url`] is a plain join and servers
/// decode `%2F`/`%5C` into separators (possibly more than once, and may
/// truncate at NUL).
pub fn has_traversal_segments(subdir: &str) -> bool {
    let mut text = subdir.to_string();
    loop {
        if text.contains('\\')
            || text.bytes().any(|b| b < 0x20 || b == 0x7f)
            || text.split('/').any(|segment| segment.trim() == "..")
        {
            return true;
        }
        let decoded = decode_percent(&text);
        if decoded == text {
            return false;
        }
        text = decoded;
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
/// Cache keys include the authenticated username when available (`u1:username@clean_url`, username
/// at its exact case) to prevent cache collisions across different user credentials accessing the
/// same WebDAV host (CWE-287 / CWE-384).
pub fn cache_key(remote_url: &str, auth_user: Option<&str>) -> String {
    // Fold case only where the URL grammar is case-insensitive (scheme,
    // host); the path selects the collection and is case-sensitive
    // (RFC 3986 6.2.2.1), so /dav/A/ and /dav/a/ must keep distinct keys.
    // The query selects the collection on query-aware endpoints (share
    // tokens, tenants), so it partitions the key exactly as sent. URL
    // userinfo never reaches here: validate_webdav_url_internal rejects
    // credentials embedded in the URL before any client exists.
    let clean_url = match reqwest::Url::parse(remote_url) {
        Ok(url) => {
            let host = url.host_str().unwrap_or_default().to_ascii_lowercase();
            // host_str() returns IPv6 literals WITHOUT brackets; re-wrap
            // them or [::1]:8080 and ::1 (or a different v6 host) collide.
            let host_display = if host.contains(':') {
                format!("[{host}]")
            } else {
                host.to_string()
            };
            let port = url.port().map(|p| format!(":{p}")).unwrap_or_default();
            // The path is kept EXACTLY as configured, trailing slash
            // included: it is part of the collection identity, and folding
            // it away would let /dav/A read or mutate /dav/A/'s entry.
            let path = url.path();
            let query = url.query().map(|q| format!("?{q}")).unwrap_or_default();
            format!("{}://{}{}{}{}", url.scheme(), host_display, port, path, query)
        }
        Err(_) => remote_url.trim_end_matches('/').to_string(),
    };
    match auth_user {
        Some(user) if !user.trim().is_empty() => {
            // Usernames are opaque, case-sensitive tokens compared by
            // exact bytes, and the key LENGTH-PREFIXES the name so a
            // username containing '@' can never collide with a different
            // (user, url) pair that concatenates to the same string.
            // The `u1:` marker versions the user-scoped key format.
            format!("u1:{}:{user}@{clean_url}", user.len())
        }
        _ => clean_url,
    }
}

/// True when a presented username names no account yet is not anonymous
/// either (empty or nothing but whitespace). Cache entry points refuse it
/// outright: an entry under it could be scoped to no credential at all, and
/// conflating it with anonymous would let one read the other's listings.
fn ambiguous_username(auth_user: Option<&str>) -> bool {
    auth_user.is_some_and(|u| u.trim().is_empty())
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
/// Equality modulo the case-insensitive parts of the URL grammar: scheme and
/// host compare case-insensitively; path (exactly as configured) and query
/// compare exactly. Ports compare via port_or_known_default, so an omitted
/// default port and its explicit spelling are the same collection here even
/// though the cache KEY keeps them distinct — key over-partitioning is
/// harmless (a miss), while comparator under-matching would cross-serve.
fn url_same_collection(a: &str, b: &str) -> bool {
    match (reqwest::Url::parse(a), reqwest::Url::parse(b)) {
        (Ok(a), Ok(b)) => {
            let host_a = a.host_str().unwrap_or_default().to_ascii_lowercase();
            let host_b = b.host_str().unwrap_or_default().to_ascii_lowercase();
            a.scheme() == b.scheme()
                && host_a == host_b
                && a.port_or_known_default() == b.port_or_known_default()
                && a.path() == b.path()
                && a.query() == b.query()
        }
        // Unparseable stored entries never match a parseable request.
        (Err(_), Err(_)) => a.trim_end_matches('/') == b.trim_end_matches('/'),
        _ => false,
    }
}

/// Equality for the cached credential: usernames are opaque tokens compared
/// by exact bytes — case and surrounding whitespace distinguish accounts on
/// case-sensitive WebDAV servers, and an absent credential (anonymous) is
/// distinct from any username, empty included.
fn same_auth_user(stored: &Option<String>, requested: Option<&str>) -> bool {
    match (stored.as_deref(), requested) {
        (None, None) => true,
        (Some(a), Some(b)) => a == b,
        _ => false,
    }
}

pub fn load_remote_cache(remote_url: &str, auth_user: Option<&str>) -> Option<Vec<WebdavItem>> {
    let path = get_cache_file_path()?;
    let cache = read_cache_map(&path);
    if ambiguous_username(auth_user) {
        return None;
    }
    let key = cache_key(remote_url, auth_user);
    let entry = cache.get(&key).filter(|entry| {
        // Key equality is necessary but not sufficient: the stored URL must
        // match the requested collection as well — scheme and host
        // case-insensitively (per URL grammar), the path exactly — and the
        // stored credential must equal the requested username exactly, so
        // no entry can ever be served for a different collection or a
        // case-variant credential.
        url_same_collection(&entry.remote_url, remote_url)
            && same_auth_user(&entry.auth_user, auth_user)
    })?;


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

    if ambiguous_username(auth_user) {
        return;
    }
    let key = cache_key(remote_url, auth_user);
    cache.insert(
        key,
        CachedRemoteListing {
            remote_url: remote_url.to_string(),
            auth_user: auth_user.map(|u| u.to_string()),
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

    if ambiguous_username(auth_user) {
        return;
    }
    let key = cache_key(remote_url, auth_user);
    if let Some(entry) = cache.get_mut(&key) {
        // Key equality alone must never authorize a mutation: the stored
        // collection and credential are rechecked exactly as load does.
        if !url_same_collection(&entry.remote_url, remote_url)
            || !same_auth_user(&entry.auth_user, auth_user)
        {
            return;
        }
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

    if ambiguous_username(auth_user) {
        return;
    }
    let key = cache_key(remote_url, auth_user);
    if let Some(entry) = cache.get_mut(&key) {
        // Key equality alone must never authorize a mutation: the stored
        // collection and credential are rechecked exactly as load does.
        if !url_same_collection(&entry.remote_url, remote_url)
            || !same_auth_user(&entry.auth_user, auth_user)
        {
            return;
        }
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

static INFLIGHT_LISTING_BYTES: Mutex<usize> = Mutex::new(0);
static INFLIGHT_LISTING_ROOM: Condvar = Condvar::new();

/// How long a scan worker parks waiting for in-flight listing capacity
/// before failing closed: real listings cycle through the budget in
/// milliseconds, so only a server keeping many worst-case bodies pinned
/// (exactly the attack the budget bounds) can exhaust this.
const INFLIGHT_LISTING_WAIT: std::time::Duration = std::time::Duration::from_secs(300);

/// RAII proof that one listing body's worst-case bytes are inside the shared
/// in-flight ceiling; dropping it returns the reservation and wakes a waiter.
pub(crate) struct InflightListingGuard {
    reserved: usize,
}

impl Drop for InflightListingGuard {
    fn drop(&mut self) {
        let mut current = INFLIGHT_LISTING_BYTES
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        *current = current.saturating_sub(self.reserved);
        drop(current);
        // One release frees exactly one worst-case body slot.
        INFLIGHT_LISTING_ROOM.notify_one();
    }
}

/// Reserves one worst-case listing body against `MAX_INFLIGHT_LISTING_BYTES`,
/// bounding the aggregate memory concurrent scan workers can pour into
/// server-controlled listing bodies. A worker that finds the ceiling full
/// WAITS for another reservation to be released — legitimate scans at every
/// thread count keep succeeding — and only fails closed after
/// `INFLIGHT_LISTING_WAIT`, at which point the caller's existing error
/// handling skips the directory or fails the action instead of buffering
/// beyond the budget.
fn reserve_inflight_listing() -> Result<InflightListingGuard, String> {
    // The reservation covers the full envelope a listing can occupy: the
    // raw body, its decoded form (worst case for UTF-16), and the parsed
    // item state — three worst-case bodies per slot.
    let want = 3 * MAX_LISTING_BODY_BYTES as usize;
    let deadline = std::time::Instant::now() + INFLIGHT_LISTING_WAIT;
    let mut current = INFLIGHT_LISTING_BYTES
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    loop {
        if let Some(next) = current.checked_add(want) {
            if next <= MAX_INFLIGHT_LISTING_BYTES {
                *current = next;
                return Ok(InflightListingGuard { reserved: want });
            }
        }
        let now = std::time::Instant::now();
        if now >= deadline {
            return Err(format!(
                "aggregate in-flight listing bytes stayed above {MAX_INFLIGHT_LISTING_BYTES} for {INFLIGHT_LISTING_WAIT:?}; too many pinned listing bodies"
            ));
        }
        let (guard, _timeout) = INFLIGHT_LISTING_ROOM
            .wait_timeout(current, deadline - now)
            .unwrap_or_else(|p| p.into_inner());
        current = guard;
    }
}

/// The body String and the in-flight reservation that covers it: the guard
/// MUST outlive the parse (the buffered body and the parser both work on it
/// inside the reserved budget), so callers keep both bindings until the
/// parsed items exist, then drop the guard.
pub(crate) struct ListingBody {
    pub(crate) body: String,
    _inflight: InflightListingGuard,
}

/// Decodes a listing body per its XML BOM: UTF-16LE/BE (which the XML
/// standard requires to carry a BOM) decode to UTF-8 in place, a UTF-8 BOM
/// is stripped, and anything else must already be valid UTF-8. Charset-less
/// guessing stays out on purpose: an undeclared non-BOM encoding fails
/// closed rather than handing the server decoding control.
fn decode_listing_body(mut bytes: Vec<u8>) -> Result<String, String> {
    if bytes.starts_with(&[0xFF, 0xFE]) || bytes.starts_with(&[0xFE, 0xFF]) {
        let big_endian = bytes[0] == 0xFE;
        bytes.drain(..2);
        if bytes.len() % 2 != 0 {
            return Err("listing body is a truncated UTF-16 sequence".to_string());
        }
        // Incremental decode straight from the byte pairs — no intermediate
        // u16 vector — with the decoded size capped BEFORE each push, so
        // peak memory is raw body + decoded body and both are inside the
        // reservation envelope.
        let mut out = String::with_capacity(bytes.len());
        let units = bytes
            .chunks_exact(2)
            .map(|p| {
                if big_endian {
                    u16::from_be_bytes([p[0], p[1]])
                } else {
                    u16::from_le_bytes([p[0], p[1]])
                }
            });
        for unit in std::char::decode_utf16(units) {
            let ch = unit.map_err(|e| format!("listing body is not valid UTF-16: {e}"))?;
            if out.len() + ch.len_utf8() > MAX_LISTING_BODY_BYTES as usize {
                return Err(format!(
                    "listing body exceeds the {MAX_LISTING_BODY_BYTES} byte limit after decoding"
                ));
            }
            out.push(ch);
        }
        return Ok(out);
    }
    if bytes.starts_with(&[0xEF, 0xBB, 0xBF]) {
        bytes.drain(..3);
    }
    String::from_utf8(bytes).map_err(|e| format!("listing body is not valid UTF-8: {e}"))
}

/// Reads a server-controlled listing body with a hard byte cap and strict
/// UTF-8 (fail closed). Unbounded buffering of a hostile response is a
/// memory-exhaustion vector; the cap turns it into a listing failure.
/// Non-UTF-8 bodies are rejected rather than charset-converted: XML
/// listings default to UTF-8, and guessing a declared charset would hand
/// the server influence over decoding, so exotic encodings fail loudly
/// and the caller falls back (Depth: 1) or errors.
pub(crate) fn read_listing_body<R: std::io::Read>(body: &mut R) -> Result<ListingBody, String> {
    // Hold the worst-case reservation for the whole read AND the parse that
    // follows it, so concurrent workers cannot each buffer a full body past
    // the shared ceiling.
    let inflight = reserve_inflight_listing()?;
    let mut limited = body.take(MAX_LISTING_BODY_BYTES + 1);
    // Read bytes, then convert in place: read_to_string would double-buffer
    // (a temporary String copy next to the internal Vec), briefly allocating
    // ~2x the body while the reservation covers only 1x.
    let mut bytes = Vec::new();
    limited
        .read_to_end(&mut bytes)
        .map_err(|e| format!("listing body read failed: {e}"))?;
    if bytes.len() as u64 > MAX_LISTING_BODY_BYTES {
        return Err(format!(
            "listing body exceeds the {MAX_LISTING_BODY_BYTES} byte limit"
        ));
    }
    let buf = decode_listing_body(bytes)?;
    // The cap must also bind the DECODED form: UTF-16 -> UTF-8 can expand
    // the text, and the budget is denominated in retained bytes.
    if buf.len() as u64 > MAX_LISTING_BODY_BYTES {
        return Err(format!(
            "listing body exceeds the {MAX_LISTING_BODY_BYTES} byte limit after decoding"
        ));
    }
    Ok(ListingBody { body: buf, _inflight: inflight })
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
pub fn parse_propfind_xml(xml: &str) -> Result<Vec<WebdavItem>, String> {
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

    // A PROPFIND response is a single multistatus document. quick_xml only
    // raises syntax errors; a body that never opens a multistatus root or
    // one truncated after a complete child element still parses "fine", so
    // the document shape is tracked here and enforced at EOF.
    let mut depth: i64 = 0;
    let mut saw_multistatus_root = false;

    let mut buf = Vec::new();

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(quick_xml::events::Event::Start(ref e)) => {
                if depth == 0 {
                    // Fail closed on document shape: the first element must
                    // be the multistatus root, and nothing may follow the
                    // closed root — a response before or after the root is
                    // not part of the document and must not inject items.
                    if saw_multistatus_root {
                        return Err(
                            "malformed PROPFIND XML: content after the multistatus root"
                                .to_string(),
                        );
                    }
                    if !e
                        .local_name()
                        .as_ref()
                        .eq_ignore_ascii_case(b"multistatus")
                    {
                        return Err(
                            "malformed PROPFIND XML: root element is not multistatus"
                                .to_string(),
                        );
                    }
                    saw_multistatus_root = true;
                }
                depth += 1;
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
                if depth == 0 {
                    // Same root-shape rules as Start: a self-closing element
                    // at document level is either the multistatus root or an
                    // error, never silent content before/after it.
                    if saw_multistatus_root {
                        return Err(
                            "malformed PROPFIND XML: content after the multistatus root"
                                .to_string(),
                        );
                    }
                    if !e
                        .local_name()
                        .as_ref()
                        .eq_ignore_ascii_case(b"multistatus")
                    {
                        return Err(
                            "malformed PROPFIND XML: root element is not multistatus"
                                .to_string(),
                        );
                    }
                    saw_multistatus_root = true;
                }
                let local = e.local_name();
                let name = local.as_ref();
                if in_resourcetype && name.eq_ignore_ascii_case(b"collection") {
                    current_is_dir = true;
                }
            }
            Ok(quick_xml::events::Event::Text(ref e)) => {
                if in_href {
                    if let Ok(text) = e.unescape() {
                        // Cap BEFORE appending: the buffer must never
                        // allocate past the limit, or one huge text node
                        // amplifies parser state past the reservation.
                        if current_href
                            .len()
                            .saturating_add(text.len())
                            > MAX_REMOTE_HREF_BYTES
                        {
                            return Err(format!(
                                "listing href exceeds the {MAX_REMOTE_HREF_BYTES} byte limit"
                            ));
                        }
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
                        if current_href
                            .len()
                            .saturating_add(text.len())
                            > MAX_REMOTE_HREF_BYTES
                        {
                            return Err(format!(
                                "listing href exceeds the {MAX_REMOTE_HREF_BYTES} byte limit"
                            ));
                        }
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
                depth = depth.saturating_sub(1);
                let local = e.local_name();
                let name = local.as_ref();
                if name.eq_ignore_ascii_case(b"response") {
                    in_response = false;
                    let clean_href = current_href.trim();
                    if !clean_href.is_empty() {
                        if clean_href.len() > MAX_REMOTE_HREF_BYTES {
                            return Err(format!(
                                "listing href exceeds the {MAX_REMOTE_HREF_BYTES} byte limit"
                            ));
                        }
                        let is_dir = current_is_dir || clean_href.ends_with('/');
                        items.push(WebdavItem {
                            href: clean_href.to_string(),
                            is_dir,
                            size: current_size,
                            mtime: current_mtime,
                        });
                        // An over-cap listing is refused outright: a
                        // truncated Ok would let duplicate-padded responses
                        // pass as complete (uniqueness is a caller concern
                        // the parser cannot see), so overflow is an error
                        // and every caller rejects the listing.
                        if items.len() > MAX_REMOTE_ITEMS {
                            return Err(format!(
                                "listing exceeded the {MAX_REMOTE_ITEMS} item limit"
                            ));
                        }
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
            Ok(quick_xml::events::Event::Eof) => {
                if !saw_multistatus_root {
                    return Err(
                        "malformed PROPFIND XML: no multistatus root element (empty or non-WebDAV body)"
                            .to_string(),
                    );
                }
                if depth != 0 {
                    return Err(format!(
                        "malformed PROPFIND XML: truncated body with {depth} unclosed element(s)"
                    ));
                }
                break;
            }
            Err(e) => {
                // A malformed document must fail the whole listing: callers
                // treat the returned set as complete ground truth (sync
                // upload/delete decisions, the listing cache). Silently
                // returning the items parsed so far turns a broken response
                // into a wrong-but-plausible listing.
                return Err(format!("malformed PROPFIND XML: {e}"));
            }
            _ => {}
        }
        buf.clear();
    }

    Ok(items)
}

/// Simple percent decoder for URI path segments.
pub fn decode_percent(s: &str) -> String {
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

/// Creates the hidden, unpredictable temporary sibling a streamed download
/// writes into before it replaces the destination. The name is fixed-form
/// and target-independent (`.scs-part-<noise>`): the noise mixes OS-seeded
/// randomness (`RandomState`), the clock, the pid, and a process-wide
/// counter, and the file is opened with `create_new`, so the path is
/// guaranteed not to exist before this call: no other file — a sibling the
/// server listed, the destination itself, or another worker's temp — can be
/// truncated by opening it, and cleanup only ever removes a file this call
/// created. Creating the temp also requires write permission on the parent
/// directory (the rename does too): a writable file inside a non-writable
/// or sticky directory, which the old in-place truncate could write, now
/// fails loudly before any byte is fetched.
fn create_unique_part_file(target: &Path) -> std::io::Result<(std::fs::File, PathBuf)> {
    use std::hash::{BuildHasher, Hasher};
    let dir = target.parent().unwrap_or_else(|| Path::new("."));
    // The replace must not widen what the old truncate path allowed: a
    // read-only (or otherwise unwritable) destination failed loudly with
    // File::create, and an atomic rename over it would silently succeed on
    // a writable directory — so write access is probed up front. Non-regular
    // destinations (directories, symlinks) are refused outright: the temp
    // scheme replaces the name itself, not what it points at.
    if let Ok(meta) = std::fs::symlink_metadata(target) {
        if !meta.is_file() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!("download destination exists and is not a regular file: {}", target.display()),
            ));
        }
        std::fs::OpenOptions::new()
            .write(true)
            .open(target)
            .map_err(|e| {
                std::io::Error::new(
                    e.kind(),
                    format!(
                        "download destination {} is not writable: {e}",
                        target.display()
                    ),
                )
            })?;
    }
    static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    for attempt in 0..64u64 {
        let mut noise = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0);
        noise ^= noise << 13;
        noise ^= noise >> 7;
        noise ^= noise << 17;
        noise ^= std::process::id() as u64;
        noise ^= std::collections::hash_map::RandomState::new().build_hasher().finish();
        noise ^= COUNTER
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            .wrapping_mul(0x9E37_79B9_7F4A_7C15)
            ^ (attempt << 32);
        // Fixed-form, target-independent name: embedding the target name
        // would push long (valid) targets past the filesystem component
        // limit, and the noise alone is both the uniqueness and the
        // unguessability.
        let candidate = dir.join(format!(".scs-part-{noise:016x}"));
        // 0o600 at creation on Unix: there is no default-mode window at
        // all, so no other local process can hold the partial body open.
        // The prior target's mode is applied just before the rename below.
        #[cfg(unix)]
        let mut options = {
            use std::os::unix::fs::OpenOptionsExt;
            let mut o = std::fs::OpenOptions::new();
            o.write(true).create_new(true).mode(0o600);
            o
        };
        #[cfg(not(unix))]
        let mut options = std::fs::OpenOptions::new();
        #[cfg(not(unix))]
        options.write(true).create_new(true);
        let file = match options.open(&candidate) {
            Ok(file) => file,
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        };
        return Ok((file, candidate));
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::AlreadyExists,
        "no unique temporary sibling available after 64 attempts",
    ))
}

/// Removes a partial temp file this call created, coping with a temp that
/// ended up read-only. Clearing the read-only bit is a Windows-only
/// fallback: on Unix, std's set_readonly(false) ORs the writable bits into
/// the mode, which would widen a 0600 partial to group/other-writable just
/// to delete it — a Unix removal failure is simply reported by doing
/// nothing further (the hidden temp stays for the user, never the data).
fn remove_part_file(temp: &Path) {
    if std::fs::remove_file(temp).is_err() {
        #[cfg(windows)]
        {
            let mut perms = std::fs::metadata(temp).map(|m| m.permissions());
            if let Ok(p) = &mut perms {
                p.set_readonly(false);
                let _ = std::fs::set_permissions(temp, p.clone());
                let _ = std::fs::remove_file(temp);
            }
        }
        #[cfg(not(windows))]
        {
            let _ = temp;
        }
    }
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

/// True when `target_url`'s path lies strictly below the configured
/// collection path of `remote_url` (exact-case; explicit default ports are
/// absorbed by origin equality). Paths that could resolve to a different
/// segment structure on the server — percent-encoded separators, dot
/// segments, duplicate slashes — never qualify, and the collection itself is
/// not an item target: only strict descendants pass.
fn is_within_collection(remote_url: &str, target_url: &str) -> bool {
    let (Ok(base), Ok(target)) = (reqwest::Url::parse(remote_url), reqwest::Url::parse(target_url))
    else {
        return false;
    };
    let (Some(base_segs), Some(target_segs)) = (
        collection_path_segments(base.path()),
        collection_path_segments(target.path()),
    ) else {
        return false;
    };
    if base_segs.is_empty() {
        // Origin-root collection: any non-root, unambiguous same-origin
        // path is a strict descendant — but the origin root itself ("/") is
        // never a valid item target.
        return !target_segs.is_empty();
    }
    // The decoded segments above only VALIDATE the path (no encoded
    // separators, dot aliases, or duplicate slashes). The prefix comparison
    // itself is byte-exact on the RAW spellings: a target like
    // `COURSE12%33` decodes onto the configured `COURSE123`, but a server
    // that treats the escape literally routes it to a sibling collection.
    let base_raw = raw_path_segments(base.path());
    let target_raw = raw_path_segments(target.path());
    target_raw.len() > base_raw.len() && target_raw[..base_raw.len()] == base_raw[..]
}

/// Raw `/`-split non-empty segments with no decoding, aligned with
/// `collection_path_segments` (which is still the validity gate: paths it
/// rejects never reach this comparison).
fn raw_path_segments(path: &str) -> Vec<&str> {
    path.split('/').filter(|seg| !seg.is_empty()).collect()
}

/// Non-empty path segments, or `None` when the path is ambiguous. Segments
/// are split on RAW '/' separators before decoding, so a percent-encoded
/// separator can never shift a segment boundary: a segment that decodes to
/// contain '/' or '\', an internal empty segment (duplicate slash), or any
/// spelling that a server could normalize into a dot segment is rejected
/// wholesale — servers may resolve any of those differently from this
/// client, so they can never be proven inside the collection. The URL
/// parser has already resolved literal dot segments, so anything dot-like
/// that appears after decoding came from encoded or aliased forms, and
/// decoding runs to a fixed point so double-encoded spellings (`%252e`) are
/// caught in the same net as single-encoded ones.
fn collection_path_segments(path: &str) -> Option<Vec<String>> {
    let raw_segs: Vec<&str> = path.split('/').collect();
    let last_idx = raw_segs.len() - 1;
    let mut segs = Vec::with_capacity(raw_segs.len());
    for (idx, raw) in raw_segs.iter().enumerate() {
        let mut decoded = decode_percent(raw);
        // Decode to a fixed point: a double-decoding server would resolve
        // `%252e` one step further each pass, so every reachable spelling
        // must be checked, not just the first decoding.
        for _ in 0..8 {
            let next = decode_percent(&decoded);
            if next == decoded {
                break;
            }
            decoded = next;
        }
        if decoded.contains('/') || decoded.contains('\\') || decoded.contains('\0') {
            return None;
        }
        if decoded.is_empty() {
            // The leading empty from an absolute path and the trailing
            // empty from a collection's slash are fine; anything else is a
            // duplicate slash, which servers may treat as distinct.
            if idx != 0 && idx != last_idx {
                return None;
            }
            continue;
        }
        if normalizes_to_dot_segment(&decoded) {
            return None;
        }
        segs.push(decoded);
    }
    Some(segs)
}

/// True for segments a server could resolve to "." or "..": the literal dot
/// segments themselves, and spellings whose trailing ASCII spaces/dots/NULs
/// (which Windows/IIS-family servers trim, so "..%20" and "..." re-alias to
/// dot segments) or path-parameter suffixes (servlet containers strip ";..."
/// suffixes, so "..;" names the parent directory) collapse to a dot segment.
fn normalizes_to_dot_segment(seg: &str) -> bool {
    fn dot_ish(s: &str) -> bool {
        let trimmed = s.trim_end_matches(&[' ', '.', '\0'][..]);
        trimmed.is_empty() || trimmed == "." || trimmed == ".."
    }
    dot_ish(seg) || dot_ish(seg.split(';').next().unwrap_or(""))
}

/// Gate for any URL taken from a server response before it becomes a request:
/// it must stay on the configured remote's origin, inside the configured
/// collection, and still pass the SSRF guard. Returns the user-facing reason
/// when the URL must not be used.
///
/// Redirect targets never reach the transport: the in-repo `dav_client`
/// builds its reqwest client with `redirect::Policy::none()`, so a 3xx sent
/// by the remote is returned as a plain non-success response instead of
/// being followed with the Basic-auth header attached.
fn guard_server_derived_url(remote_url: &str, target_url: &str) -> Result<(), String> {
    if !same_remote_origin(remote_url, target_url) {
        return Err("href resolves outside the configured remote origin".to_string());
    }
    if !is_within_collection(remote_url, target_url) {
        return Err("href resolves outside the configured collection directory".to_string());
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

/// Path-segment spellings the WHATWG URL parser collapses into dot segments
/// (`.` removed, `..` pops the parent) even though they contain no literal
/// `.` component: `%2e`, `.%2e`, `%2e.`, `%2e%2e` (ASCII case-insensitive).
/// The collapse happens inside `Url::parse`, so the raw string this guard
/// inspects is the only form where the original spelling is still visible.
fn has_url_dot_segment(raw: &str) -> bool {
    // Apply the WHATWG parser's input normalization first: it trims
    // leading/trailing C0 control or space and removes all ASCII tab, LF,
    // and CR anywhere in the input, so `https://h/base/..\n` reaches the
    // parser as the segment `..`. The guard must examine that same
    // normalized form, or the parent pop slips past the raw check.
    let normalized: String = raw
        .trim_matches(|c: char| (c as u32) <= 0x20)
        .chars()
        .filter(|&c| !matches!(c, '\t' | '\n' | '\r'))
        .collect();
    // The scheme ends at the FIRST ':' only: a '://' inside the query or
    // fragment must never be mistaken for the scheme delimiter (it would
    // hide real dot segments or falsely reject query text as traversal).
    // An optional '//' — or '\\' for special schemes — follows and is not
    // part of the authority.
    let after_scheme = match normalized.split_once(':') {
        Some((_, rest)) => rest.trim_start_matches(['/', '\\']),
        None => &normalized,
    };
    // A backslash directly after the authority is the path start too: the
    // WHATWG parser accepts `\` as a path separator for special schemes, so
    // `https://host\..\x` parses with a path of `/x` after the parent pop.
    let path_start = after_scheme
        .find(['/', '\\', '?', '#'])
        .unwrap_or(after_scheme.len());
    let path_and_rest = &after_scheme[path_start..];
    let path = path_and_rest
        .split(['?', '#'])
        .next()
        .unwrap_or("");
    path.split(['/', '\\']).any(|seg| {
        let s = seg.to_ascii_lowercase();
        // Windows/IIS-family servers trim trailing ASCII spaces and dots
        // (including the %20 spelling of space) from each path component, so
        // a spelling like `.. `, `...`, or `..%20` resolves to a dot segment
        // at the server even though the exact matches below do not see one:
        // strip those suffixes first and reject anything that collapses to
        // a dot form (or to nothing but dots and spaces).
        // After the trailing trim, EVERY dot-form spelling — literal,
        // mixed, or fully encoded ("%2e" is itself a trimmable suffix) —
        // collapses to the empty check; nothing survives for exact matches.
        let trimmed = trim_windows_alias_suffixes(&s);
        (!s.is_empty() && trimmed.is_empty())
            // Encoded separators can decode back into path separators
            // server-side (a raw-concatenated `..%2fsecret` subdir), so a
            // configured scope never carries them.
            || s.contains("%2f")
            || s.contains("%5c")
    })
}

/// Strips any mix of trailing literal spaces, dots, and `%20` from a
/// (lowercased) path segment: the per-component trim Windows-family servers
/// apply before resolving the path.
fn trim_windows_alias_suffixes(mut s: &str) -> &str {
    // `s` arrives lowercased, so the encoded spellings need no case handling.
    loop {
        if s.ends_with(' ') || s.ends_with('.') {
            s = &s[..s.len() - 1];
        } else if let Some(stripped) = s.strip_suffix("%20").or_else(|| s.strip_suffix("%2e")) {
            s = stripped;
        } else {
            return s;
        }
    }
}

pub fn validate_webdav_url_internal(url_str: &str, allow_insecure: bool) -> Result<(), String> {
    let url = reqwest::Url::parse(url_str)
        .map_err(|e| format!("Invalid WebDAV URL: {}", e))?;

    // Reject dot-segment spellings in their raw form: `Url::parse` collapses
    // `..`/`.%2e`/`%2e.`/`%2e%2e` path segments (and drops `%2e`), silently
    // shifting the effective remote scope above the configured base path.
    if has_url_dot_segment(url_str) {
        return Err(
            "URL path contains a dot-segment ('.', '..' or percent-encoded form) or an encoded path separator ('%2f'/'%5c') that would resolve above the configured directory."
                .to_string(),
        );
    }

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
        Ok(mut res) => {
            let status = res.status();
            if status.is_success() || status.as_u16() == 207 {
                // A failed body read must not degrade into an empty listing:
                // it flows through the same fallback as a malformed body, and
                // the body itself is read through the bounded reader.
                let parsed: Result<Vec<WebdavItem>, String> = read_listing_body(&mut res)
                    .and_then(|listing| {
                        // Parse inside the reservation: the body and its
                        // parse both belong to the in-flight budget.
                        let parsed = parse_propfind_xml(&listing.body);
                        drop(listing);
                        parsed
                    });
                let items: Vec<WebdavItem> = match parsed {
                    Ok(items) => items,
                    Err(reason) => {
                        // A malformed listing must not be treated as a
                        // complete result: fall back to the Depth:1 traversal
                        // below instead of trusting a partial parse.
                        log(&format!(
                            "{reason}; falling back to Depth: 1 traversal...\n"
                        ));
                        Vec::new()
                    }
                }
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
                // Per-listing item caps are enforced by the parser itself:
                // an over-cap listing is a parse error on every path.
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
    // Kept for its validation side effect: an unparsable remote must fail the
    // scan before any worker is spawned.
    let _ = reqwest::Url::parse(remote_url).map_err(|e| format!("Invalid remote URL: {e}"))?;

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
        /// Retained bytes of every accepted href: a whole-scan budget next
        /// to the per-listing item cap.
        retained_listing_bytes: usize,
        /// Items processed across all listings, duplicates included.
        processed_listing_items: usize,
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
        retained_listing_bytes: 0,
        processed_listing_items: 0,
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
        cancel_flag: &'env AtomicBool,
        state_ref: &'env Mutex<ScanState>,
        cvar_ref: &'env Condvar,
        exclude_folders: &'env [String],
        log_tx: mpsc::Sender<String>,
    ) {
        let worker_log_tx = log_tx;
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

                    let mut res = res;
                    let listing = match read_listing_body(&mut res) {
                        Ok(listing) => listing,
                        Err(reason) => {
                            // A capacity timeout parks this directory only:
                            // other workers keep scanning, and the slow
                            // listing is reported as skipped rather than
                            // aborting the whole action.
                            if reason.contains("in-flight") {
                                let mut state =
                                    state_ref.lock().unwrap_or_else(|p| p.into_inner());
                                state.skipped_dirs.push(current_url.clone());
                                drop(state);
                                let _ = worker_log_tx.send(format!(
                                    "[{}] WARNING: skipping {}: {}\n",
                                    scan_idx, display_folder, reason
                                ));
                                continue;
                            }
                            let mut state = state_ref.lock().unwrap_or_else(|p| p.into_inner());
                            state.error = Some(format!("PROPFIND {current_url}: {reason}"));
                            state.stopped = true;
                            return;
                        }
                    };
                    let items = match parse_propfind_xml(&listing.body) {
                        Ok(items) => items,
                        Err(reason) => {
                            let mut state = state_ref.lock().unwrap_or_else(|p| p.into_inner());
                            state.error = Some(format!("PROPFIND {current_url}: {reason}"));
                            state.stopped = true;
                            return;
                        }
                    };
                    // The in-flight reservation lives until end of scope:
                    // the 0-children preview below still reads the body, and
                    // releasing a moment later only delays, never exceeds,
                    // the shared budget.
                    if items.is_empty() {
                        // A Depth: 1 listing always contains at least the
                        // collection itself; zero responses means the body
                        // was never a real listing and must not be trusted as
                        // "directory is empty" ground truth for deletes.
                        let mut state = state_ref.lock().unwrap_or_else(|p| p.into_inner());
                        state.error = Some(format!(
                            "PROPFIND returned an empty multistatus for {current_url}"
                        ));
                        state.stopped = true;
                        return;
                    }

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
                            if same_remote_origin(remote_url, &item_url)
                                && !rel_from_root.is_empty()
                                && is_safe_relative_path(&rel_from_root)
                                && validate_webdav_url(&item_url).is_ok()
                                && is_within_collection(remote_url, &item_url)
                            {
                                discovered_sub_urls.push(item_url);
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
                        let preview: String = listing.body.chars().take(400).collect();
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
                            state.processed_listing_items += 1;
                            if state.processed_listing_items > MAX_PROCESSED_LISTING_ITEMS {
                                state.error = Some(format!(
                                    "Remote scan processed more than {MAX_PROCESSED_LISTING_ITEMS} listing items (duplicates included). Aborting for security."
                                ));
                                state.stopped = true;
                                return;
                            }
                            let key = (item.is_dir, item.href.clone());
                            if state.seen_items.insert(key) {
                                if state.all_items.len() >= MAX_REMOTE_ITEMS {
                                    state.error = Some(format!(
                                        "Remote listing exceeded the {MAX_REMOTE_ITEMS} item limit. Aborting for security."
                                    ));
                                    state.stopped = true;
                                    return;
                                }
                                // Both retained copies count (all_items'
                                // String and the seen_items key clone), plus
                                // fixed per-entry container overhead.
                                state.retained_listing_bytes +=
                                    2 * item.href.len() + RETAINED_ITEM_OVERHEAD_BYTES;
                                if state.retained_listing_bytes > MAX_RETAINED_LISTING_BYTES {
                                    state.error = Some(format!(
                                        "Remote listing retained bytes exceeded the {MAX_RETAINED_LISTING_BYTES} byte budget. Aborting for security."
                                    ));
                                    state.stopped = true;
                                    return;
                                }
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

/// A remote folder paired with the total byte size of every file beneath it.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct RemoteFolder {
    /// Slash-separated path relative to the listed collection; "" is the collection itself.
    pub path: String,
    /// Sum of file sizes anywhere beneath this folder (0 when it holds no files).
    pub size: u64,
}

/// Aggregates a recursive listing of `remote_url` into one [`RemoteFolder`] per
/// folder, sorted by path with the root first.
///
/// Each file's size is credited to the root and to every folder it is nested in;
/// empty folders still appear with size 0; files themselves never appear.
pub fn aggregate_remote_folders(remote_url: &str, items: &[WebdavItem]) -> Vec<RemoteFolder> {
    // BTreeMap keeps output sorted by path, with "" (the root) first.
    let mut sizes: std::collections::BTreeMap<String, u64> = std::collections::BTreeMap::new();
    sizes.entry(String::new()).or_insert(0);
    for item in items {
        let rel = relative_item_path(remote_url, &item.href);
        let segments: Vec<&str> = rel.split('/').filter(|s| !s.is_empty()).collect();
        if segments.is_empty() {
            continue;
        }
        if item.is_dir {
            // Register the folder (and, defensively, every ancestor).
            let mut acc = String::new();
            for segment in &segments {
                if !acc.is_empty() {
                    acc.push('/');
                }
                acc.push_str(segment);
                sizes.entry(acc.clone()).or_insert(0);
            }
        } else {
            // Credit the root and every ancestor folder; never the file itself.
            *sizes.entry(String::new()).or_insert(0) += item.size;
            let mut acc = String::new();
            for segment in &segments[..segments.len() - 1] {
                if !acc.is_empty() {
                    acc.push('/');
                }
                acc.push_str(segment);
                *sizes.entry(acc.clone()).or_insert(0) += item.size;
            }
        }
    }
    sizes
        .into_iter()
        .map(|(path, size)| RemoteFolder { path, size })
        .collect()
}

/// Lists every folder beneath `remote_url` with its aggregated size, using the
/// shared recursive listing machinery (retries, worker pool, listing cache).
///
/// Exclusions are deliberately not applied so already-excluded folders stay
/// visible and selectable in the picker this feeds. Only PROPFIND metadata is
/// requested — no file contents are ever downloaded.
pub fn list_remote_folders<F>(
    client: &Client,
    remote_url: &str,
    cancel_flag: &AtomicBool,
    concurrency: Option<usize>,
    auth_user: Option<&str>,
    log: F,
) -> Result<Vec<RemoteFolder>, String>
where
    F: FnMut(&str),
{
    let items = list_remote_recursive_with_exclusions_and_log(
        client,
        remote_url,
        cancel_flag,
        concurrency,
        auth_user,
        true,
        &[],
        log,
    )?;
    Ok(aggregate_remote_folders(remote_url, &items))
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
            let mut res = res;
            let listing = read_listing_body(&mut res)?;
            let items = parse_propfind_xml(&listing.body)?;
            drop(listing);
            // Per-listing item caps are enforced by the parser itself.
            if items.is_empty() {
                return Err(
                    "PROPFIND returned an empty multistatus (no resources, not even the collection)"
                        .to_string(),
                );
            }
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
                        // Stream to a hidden, unpredictable temporary sibling
                        // and replace the destination only after the body
                        // completed: File::create would truncate the user's
                        // existing copy before the first body byte arrives,
                        // and a PREDICTABLE temp name could collide with a
                        // real sibling file (a hostile listing can contain
                        // `foo.scs-part-<pid>`) and truncate/delete that
                        // instead. create_new guarantees the path did not
                        // exist, so cleanup only ever removes this call's own
                        // file — every failure exit below does exactly that,
                        // because the random name cannot be recomputed later.
                        let (mut dest_file, temp_file) = create_unique_part_file(&target_file)
                            .map_err(|e| {
                                AttemptError::Fatal(format!(
                                    "Failed to create a temporary sibling of {}: {}",
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
                                // The outer error path checks the flag again
                                // and ends the operation as a user
                                // cancellation; this exit owns the temp file.
                                drop(dest_file);
                                remove_part_file(&temp_file);
                                return Err(AttemptError::Fatal(
                                    "Operation canceled by user.".to_string(),
                                ));
                            }
                            if std::time::Instant::now() >= body_deadline {
                                drop(dest_file);
                                remove_part_file(&temp_file);
                                return Err(AttemptError::Fatal(format!(
                                    "Download body exceeded the {}s time limit",
                                    get_download_body_timeout_secs()
                                )));
                            }
                            use std::io::Read as _;
                            match limited_reader.read(&mut buffer) {
                                Ok(0) => break,
                                Ok(n) => {
                                    if let Err(e) = dest_file.write_all(&buffer[..n]) {
                                        // Close before remove so Windows can
                                        // delete this call's own temp file.
                                        drop(dest_file);
                                        remove_part_file(&temp_file);
                                        return Err(classify_io_error(
                                            &format!("writing GET {download_url}"),
                                            &e,
                                        ));
                                    }
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
                                    drop(dest_file);
                                    remove_part_file(&temp_file);
                                    return Err(classify_io_error(
                                        &format!("streaming GET {download_url}"),
                                        &e,
                                    ));
                                }
                            }
                        }
                        use std::io::Write as _;
                        if written > max_size {
                            drop(dest_file);
                            remove_part_file(&temp_file);
                            return Err(AttemptError::Fatal(format!(
                                "Downloaded file exceeded the maximum size limit ({} bytes > {} bytes)",
                                written, max_size
                            )));
                        }
                        // The listing advertised a length: a body that ends
                        // cleanly but SHORT (a hostile chunked response does
                        // exactly that) must not replace the prior file with
                        // truncated content. A LONGER body is accepted — the
                        // remote file may legitimately have grown since the
                        // listing — and an unknown/zero advertised size skips
                        // the comparison entirely.
                        if item.size > 0 && written < item.size as u64 {
                            drop(dest_file);
                            remove_part_file(&temp_file);
                            return Err(AttemptError::Fatal(format!(
                                "Downloaded {} bytes but the listing advertised {} for {download_url}",
                                written, item.size
                            )));
                        }
                        // The replaced file keeps the prior file's Unix
                        // OWNER mode bits, applied only now, after the body
                        // is fully written, so a read-only mode can never
                        // block the stream. Known regression vs the
                        // old truncate-in-place path, documented in the PR:
                        // an atomic rename swaps the inode, so owner/group,
                        // xattrs, POSIX ACLs, and Windows security
                        // descriptors are NOT carried over (the old
                        // File::create wrote into the SAME inode and kept
                        // them). Mode bits are what std can preserve without
                        // platform-specific metadata APIs. New downloads
                        // (no prior file) intentionally land 0600: private
                        // by default, instead of the umask default.
                        #[cfg(unix)]
                        if let Ok(prior) = std::fs::symlink_metadata(&target_file) {
                            // Only a still-regular destination contributes a
                            // mode: anything swapped in mid-flight (a symlink
                            // would carry 0777) must not widen the temp.
                            if prior.is_file() {
                                let mode = {
                                    use std::os::unix::fs::PermissionsExt;
                                    // Owner bits only: the replace loses the
                                    // original group/owner, so keeping
                                    // group/other bits would grant them to
                                    // whoever the new group happens to be.
                                    std::fs::Permissions::from_mode(
                                        prior.permissions().mode() & 0o700,
                                    )
                                };
                                // Handle-based: a path-based chmod would
                                // follow a symlink swapped onto the temp
                                // path; the open handle cannot be swapped.
                                dest_file.set_permissions(mode).map_err(
                                    |e| {
                                        remove_part_file(&temp_file);
                                        AttemptError::Fatal(format!(
                                            "Failed to apply the prior file's permissions to {}: {e}",
                                            temp_file.display()
                                        ))
                                    },
                                )?;
                            }
                        }
                        // Atomic replace: the destination is only ever touched
                        // with a complete, size-checked body. std's rename
                        // replaces an existing destination file on Windows as
                        // well (MoveFileExW with MOVEFILE_REPLACE_EXISTING).
                        drop(dest_file);
                        std::fs::rename(&temp_file, &target_file).map_err(|e| {
                            remove_part_file(&temp_file);
                            AttemptError::Fatal(format!(
                                "Failed to finalize {}: {}",
                                target_file.display(),
                                e
                            ))
                        })?;
                        Ok(written)
                    },
                );
                let downloaded_len = match download_result {
                    Ok(len) => len,
                    Err(e) => {
                        // Never leave a partial behind after the final
                        // attempt: every attempt already removed its own
                        // temp sibling on failure, and the destination was
                        // never touched, so the prior local file survives a
                        // failed download as-is.
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

        let items = parse_propfind_xml(sample).unwrap();
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

        let items = parse_propfind_xml(xml).unwrap();
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
        let items = parse_propfind_xml(xml).unwrap();
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

    /// Serializes the tests that touch the shared in-flight budget or the
    /// bounded reader, so a parallel `cargo test` cannot interleave them.
    static LISTING_BUDGET_TEST_MUTEX: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[test]
    fn test_listing_body_is_byte_bounded() {
        let _guard = LISTING_BUDGET_TEST_MUTEX.lock().unwrap_or_else(|p| p.into_inner());
        // Server-controlled listing bodies must never be buffered without a
        // bound: a response larger than the cap is an error, not a String.
        let over: String = "x".repeat(MAX_LISTING_BODY_BYTES as usize + 1);
        let mut cursor = std::io::Cursor::new(over.into_bytes());
        assert!(read_listing_body(&mut cursor).is_err());

        let under: String = "x".repeat(1024);
        let mut cursor = std::io::Cursor::new(under.clone().into_bytes());
        assert_eq!(read_listing_body(&mut cursor).unwrap().body, under);

        // Invalid UTF-8 is a read error (fail closed), not lossy acceptance.
        let mut cursor = std::io::Cursor::new(vec![0xff, 0xfe, 0x00]);
        assert!(read_listing_body(&mut cursor).is_err());
    }

    #[test]
    fn test_cache_key_does_not_fold_url_path_case() {
        // URL paths are case-sensitive (RFC 3986 6.2.2.1): two collections
        // differing only in path case are distinct remotes and must never
        // share a listing-cache entry (a shared entry steers sync's
        // delete set at the wrong collection).
        assert_ne!(
            cache_key("https://h.example/dav/A/", Some("u")),
            cache_key("https://h.example/dav/a/", Some("u"))
        );
        // Scheme and host remain case-insensitive by URL grammar.
        assert_eq!(
            cache_key("https://H.EXAMPLE/dav/A/", Some("u")),
            cache_key("https://h.example/dav/A/", Some("u"))
        );
        // Host case folding must not leak into the path.
        assert_ne!(
            cache_key("https://h.example/dav/A/", Some("u")),
            cache_key("https://H.example/dav/a/", Some("u"))
        );
    }

    #[test]
    fn test_validate_webdav_url_rejects_parser_normalized_traversal() {
        // Url::parse trims C0 control/space at both ends and drops ASCII
        // tab/LF/CR anywhere, so these raw spellings reach the parser as a
        // parent-pop `..` (or encoded `%2e%2e`) segment even though the raw
        // string never contains one literally. The error message pins the
        // rejection to the dot-segment guard.
        for raw in [
            "https://courselinkdav.desire2learn.com/base/..\n",
            "https://courselinkdav.desire2learn.com/base/.. ",
            "https://courselinkdav.desire2learn.com/base/.\t.",
            "https://courselinkdav.desire2learn.com/base/\n..",
            "https://courselinkdav.desire2learn.com/base/%2e%2e\n",
        ] {
            let err = validate_webdav_url_internal(raw, false).unwrap_err();
            assert!(
                err.contains("dot-segment"),
                "{raw:?} must be rejected as a dot-segment, got: {err}"
            );
        }
        // Encoded separators decode back into path separators server-side,
        // so a configured scope never carries them.
        for raw in [
            "https://courselinkdav.desire2learn.com/dav/..%2fsecret",
            "https://courselinkdav.desire2learn.com/dav/..%5csecret",
        ] {
            let err = validate_webdav_url_internal(raw, false).unwrap_err();
            assert!(
                err.contains("encoded path separator"),
                "{raw} must be rejected as an encoded separator, got: {err}"
            );
        }
        // Edge whitespace alone is normalized away and stays accepted.
        assert!(validate_webdav_url_internal(
            "  https://courselinkdav.desire2learn.com/dav/my%20course  ",
            false
        )
        .is_ok());
    }

    #[test]
    fn test_validate_webdav_url_rejects_backslash_after_authority() {
        // The WHATWG parser treats a backslash as a path separator for
        // special schemes, so `https://host\..\x` parses with a path of
        // `/x` after the parent pop. The guard must recognize the backslash
        // as the path start rather than part of the authority, or the
        // traversal escapes the configured scope unseen. The error message
        // pins the rejection to the dot-segment guard itself.
        for raw in [
            r"https://courselinkdav.desire2learn.com\..\secret",
            r"https://courselinkdav.desire2learn.com:8443\..\secret",
            r"https://courselinkdav.desire2learn.com\%2e%2e/secret",
            r"https://courselinkdav.desire2learn.com\.\secret",
        ] {
            let err = validate_webdav_url_internal(raw, false).unwrap_err();
            assert!(
                err.contains("dot-segment"),
                "{raw} must be rejected as a dot-segment, got: {err}"
            );
        }
        // A backslash separating ordinary segments is a path separator, not
        // a traversal, and stays accepted.
        assert!(validate_webdav_url_internal(
            r"https://courselinkdav.desire2learn.com\dav\",
            false
        )
        .is_ok());
    }

    #[test]
    fn test_validate_webdav_url_rejects_encoded_dot_segments() {
        // The WHATWG URL parser treats these path segments as double-dot
        // segments and pops the parent directory, so a literal '.'/'..'
        // check never sees them: the remote scope would silently shift
        // above the configured base path.
        for path in [
            "/dav/%2e%2e/",
            "/dav/.%2e/",
            "/dav/%2e./",
            "/dav/notes/%2e%2e",
            "/dav/%2E%2E/",
        ] {
            assert!(
                validate_webdav_url_internal(&format!("https://courselinkdav.desire2learn.com{path}"), false).is_err(),
                "encoded dot-dot path segment must be rejected: {path}"
            );
        }
        // The encoded single-dot form is silently dropped by the same rule.
        assert!(validate_webdav_url_internal("https://courselinkdav.desire2learn.com/dav/%2e", false).is_err());
        // Ordinary percent-encoded characters stay allowed.
        assert!(validate_webdav_url_internal("https://courselinkdav.desire2learn.com/dav/my%20course/", false).is_ok());
    }

    #[test]
    fn test_has_url_dot_segment_splits_scheme_only_at_the_first_colon() {
        // A '://' inside the query or fragment must never be mistaken for
        // the scheme delimiter: the first form hides a real dot segment,
        // the second buries traversal-looking text in the query where the
        // parser puts it.
        assert!(
            has_url_dot_segment("https:host/..?x=://"),
            "dot segments before a query-borne '://' must still be caught"
        );
        assert!(
            !has_url_dot_segment("https:host?x=://../.."),
            "traversal-looking QUERY text is not a path dot segment"
        );
        assert!(has_url_dot_segment("https://h.example/dav/../x"));
        assert!(!has_url_dot_segment("https://h.example/dav/x?return=https://other/p"));
    }

    #[test]
    fn test_validate_webdav_url_rejects_windows_trim_dot_aliases() {
        // Windows/IIS-family servers strip trailing ASCII spaces and dots
        // (including their %20 spelling) from each path component, so these
        // alias onto "." / ".." at resolve time even though an exact-match
        // dot check passes them. The trailing trim must also strip %2e:
        // `..%20%2e` decodes to `.. .` and trims to `..`.
        for path in [
            "/dav/.. /",
            "/dav/.../",
            "/dav/..%20/",
            "/dav/. /",
            "/dav/notes/..%20/x",
            "/dav/..%20%2e/",
            "/dav/..%2e/",
            "/dav/notes/..%2E/x",
            "/dav/%2e%2e%2e/",
        ] {
            assert!(
                validate_webdav_url_internal(&format!("https://courselinkdav.desire2learn.com{path}"), false).is_err(),
                "windows-trim dot alias must be rejected: {path}"
            );
        }
        // A name that merely ends in a dot — literal or encoded — is not a
        // dot segment.
        assert!(validate_webdav_url_internal("https://courselinkdav.desire2learn.com/dav/report./", false).is_ok());
        assert!(validate_webdav_url_internal("https://courselinkdav.desire2learn.com/dav/report%2e/", false).is_ok());
    }

    #[test]
    fn test_build_remote_url_scope_cannot_escape_via_encoded_subdir() {
        // build_remote_url concatenates the user-supplied subdir into the
        // remote URL; the guard must reject the result before any request.
        for subdir in ["%2e%2e", "notes/.%2e", "notes/%2e%2e/sub"] {
            let url = build_remote_url("https://courselinkdav.desire2learn.com/content/enforced/1234", subdir);
            assert!(
                validate_webdav_url_internal(&url, false).is_err(),
                "subdir {subdir:?} must not shift the remote scope"
            );
        }
        let url = build_remote_url("https://courselinkdav.desire2learn.com/content/enforced/1234", "notes");
        assert!(validate_webdav_url_internal(&url, false).is_ok());
    }

    #[test]
    fn test_parse_propfind_xml_rejects_malformed_tail() {
        // A malformed tail must fail the whole listing, not silently return
        // the items parsed so far: sync treats the returned set as ground
        // truth (files missing from it get re-uploaded unconditionally, and
        // the truncated set is cached for 24h).
        let body = concat!(
            "<?xml version=\"1.0\" encoding=\"utf-8\" ?>",
            "<D:multistatus xmlns:D=\"DAV:\">",
            "<D:response><D:href>/c/a.txt</D:href><D:propstat><D:prop>",
            "<D:resourcetype/><D:getcontentlength>3</D:getcontentlength>",
            "</D:prop><D:status>HTTP/1.1 200 OK</D:status></D:propstat></D:response>",
            "<D:response><D:href>/c/b.txt</D:href><D:propstat><D:prop>",
            "<D:resourcetype/><D:getcontentlength>3</D:getcontentlength>",
            "</D:prop><D:status>HTTP/1.1 200 OK</D:status></D:propstat></D:response>",
            "<D:response><D:href>/c/c.txt</D:href><D:propstat><D:prop>",
            "<D:resourcetype/><D:getcontentlength>3</D:getcontentlength>",
            "</D:prop><D:status>HTTP/1.1 200 OK</D:status></D:propstat></D:respo",
        );
        let parsed = parse_propfind_xml(body);
        assert!(
            parsed.is_err(),
            "malformed XML tail must be an error, not a silent 2-item listing"
        );
        // Complete only the truncated tail. Appending — instead of replacing
        // "</D:respo", which also matches inside the first two blocks'
        // "</D:response>" close tags — leaves the well-formed responses
        // untouched.
        let ok_body = format!("{body}nse></D:multistatus>");
        assert_eq!(parse_propfind_xml(&ok_body).unwrap().len(), 3);
    }

    #[test]
    fn test_parse_propfind_xml_requires_multistatus_root() {
        // An empty body (e.g. a defaulted body read) or well-formed
        // non-multistatus XML (proxied HTML error pages) must not parse as
        // an empty-but-complete listing: callers treat Ok(vec![]) as ground
        // truth and act on everything "missing" from it.
        assert!(
            parse_propfind_xml("").is_err(),
            "empty body must be an error, not an empty listing"
        );
        assert!(
            parse_propfind_xml("<html><body><h1>404 Not Found</h1></body></html>").is_err(),
            "well-formed non-multistatus XML must be an error"
        );
        assert!(
            parse_propfind_xml(
                "<D:response><D:href>/c/a.txt</D:href><D:propstat><D:prop>\
                 <D:resourcetype/></D:prop><D:status>HTTP/1.1 200 OK</D:status>\
                 </D:propstat></D:response>"
            )
            .is_err(),
            "response without a multistatus root must be an error"
        );
        // Elements BEFORE the root are not part of the document: a response
        // ahead of a real multistatus root must fail the parse outright, not
        // inject its items into the listing.
        assert!(
            parse_propfind_xml(
                "<D:response><D:href>/c/a.txt</D:href><D:propstat><D:prop>\
                 <D:resourcetype/></D:prop><D:status>HTTP/1.1 200 OK</D:status>\
                 </D:propstat></D:response>\
                 <D:multistatus xmlns:D=\"DAV:\"><D:response><D:href>/c/b.txt</D:href>\
                 <D:propstat><D:prop><D:resourcetype/></D:prop>\
                 <D:status>HTTP/1.1 200 OK</D:status></D:propstat></D:response>\
                 </D:multistatus>"
            )
            .is_err(),
            "a response before the multistatus root must be an error"
        );
        // Nothing may follow the closed root either.
        assert!(
            parse_propfind_xml(
                "<D:multistatus xmlns:D=\"DAV:\"></D:multistatus><D:multistatus xmlns:D=\"DAV:\"/>"
            )
            .is_err(),
            "a second root after the closed multistatus must be an error"
        );
    }

    #[test]
    fn test_cache_key_keeps_query_distinct() {
        // Queries select distinct collections on query-aware WebDAV
        // endpoints (share tokens, tenants), so they must partition the
        // key; the path's insignificant trailing slash must not.
        assert_ne!(
            cache_key("https://h.example/dav?share=a", Some("u")),
            cache_key("https://h.example/dav?share=b", Some("u"))
        );
        assert_ne!(
            cache_key("https://h.example/dav/?share=a", Some("u")),
            cache_key("https://h.example/dav?share=a", Some("u")),
            "the trailing slash is part of the collection identity"
        );
        assert_eq!(
            cache_key("https://h.example/dav?share=a", Some("u")),
            cache_key("https://h.example/dav?share=a", Some("u"))
        );
    }

    #[test]
    fn test_inflight_listing_budget_bounds_concurrent_bodies() {
        let _guard = LISTING_BUDGET_TEST_MUTEX.lock().unwrap_or_else(|p| p.into_inner());
        // The per-body cap alone would let MAX_SCAN_CONCURRENCY workers each
        // buffer a full MAX_LISTING_BODY_BYTES body (~4 GiB aggregate) before
        // any aggregate check runs: every listing read reserves its worst
        // case against a shared ceiling. A worker that finds the ceiling
        // full WAITS for capacity (scans at every thread count must still
        // succeed) and only fails closed once the wait budget is exhausted.
        let max_guards = MAX_INFLIGHT_LISTING_BYTES / (3 * MAX_LISTING_BODY_BYTES as usize);
        let mut held: Vec<InflightListingGuard> = Vec::new();
        for _ in 0..max_guards {
            held.push(
                reserve_inflight_listing()
                    .expect("reservation within the ceiling must succeed"),
            );
        }
        // Ceiling full: a waiter must park instead of failing or overbooking.
        let waiter = std::thread::spawn(|| reserve_inflight_listing().is_ok());
        std::thread::sleep(std::time::Duration::from_millis(300));
        assert!(
            !waiter.is_finished(),
            "a waiter must block while the ceiling is full"
        );
        // Releasing one reservation must admit the waiter.
        drop(held.pop().expect("guards held"));
        assert!(
            waiter.join().expect("waiter must not panic"),
            "the waiter must acquire after a release"
        );
        drop(held);
        assert!(
            reserve_inflight_listing().is_ok(),
            "released reservations must be reusable"
        );
    }

    #[test]
    fn test_parse_propfind_xml_rejects_truncation_after_complete_response() {
        // quick_xml raises no syntax error for XML that simply stops after a
        // complete </D:response>: unclosed ancestors are only detectable at
        // EOF, so the parser must track element depth itself.
        let body = concat!(
            "<?xml version=\"1.0\" encoding=\"utf-8\" ?>",
            "<D:multistatus xmlns:D=\"DAV:\">",
            "<D:response><D:href>/c/a.txt</D:href><D:propstat><D:prop>",
            "<D:resourcetype/><D:getcontentlength>3</D:getcontentlength>",
            "</D:prop><D:status>HTTP/1.1 200 OK</D:status></D:propstat></D:response>",
        );
        let parsed = parse_propfind_xml(body);
        assert!(
            parsed.is_err(),
            "truncation after a complete response must be an error, not a 1-item listing"
        );
        let ok_body = format!("{body}</D:multistatus>");
        assert_eq!(parse_propfind_xml(&ok_body).unwrap().len(), 1);
    }

    #[test]
    fn test_guard_server_derived_url_bounds_targets_to_the_collection() {
        // The user confirms an action for ONE collection (base_url + subdir).
        // A server-advertised href may point anywhere on the same origin; the
        // guard must refuse to let it steer authenticated DELETE/GET outside
        // the configured collection.
        let base = "https://h.example/content/enforced/COURSE123/";
        assert!(
            guard_server_derived_url(base, "https://h.example/othercourse/file.txt").is_err(),
            "same-origin out-of-collection href must be refused"
        );
        assert!(
            guard_server_derived_url(base, "/othercourse/file.txt").is_err(),
            "relative out-of-collection href must be refused"
        );
        // Strict descendants only: the collection itself is never a valid
        // item-level request target, and case-folded spellings name a
        // different collection on case-sensitive servers.
        assert!(
            guard_server_derived_url(base, "https://h.example/content/enforced/COURSE123/").is_err(),
            "the collection root itself must not pass as an item target"
        );
        assert!(
            guard_server_derived_url(base, "https://h.example/Content/Enforced/course123/notes/a.txt").is_err(),
            "case-folded hrefs must not match the configured collection"
        );
        // Encoded separators and dot segments must not create segment
        // boundaries the guard would trust.
        assert!(
            guard_server_derived_url(base, "https://h.example/content/enforced/COURSE123%2F..%2Fothercourse/file.txt").is_err(),
            "percent-encoded separators decoding into traversal must be refused"
        );
        assert!(
            guard_server_derived_url(base, "https://h.example/content/enforced/COURSE123/..%2f..%2fothercourse/file.txt").is_err(),
            "encoded dot segments must be refused"
        );
        assert!(
            guard_server_derived_url(base, "https://h.example/content/enforced/COURSE123/sub%5C..%5Cothercourse/file.txt").is_err(),
            "encoded backslash traversal must be refused"
        );
        assert!(
            guard_server_derived_url(base, "https://h.example/content/enforced/COURSE123/%2e%2e/othercourse/file.txt").is_err(),
            "encoded dot-dot segments must be refused"
        );
        // Even a non-dot encoded separator stays inside one raw segment and
        // is rejected: a server that does not decode %2F would route it to a
        // different path than the one compared here.
        assert!(
            guard_server_derived_url(base, "https://h.example/content/enforced/COURSE123/notes%2Fextra.txt").is_err(),
            "encoded separators must never be treated as segment boundaries"
        );
        // Duplicate slashes may name a different path on the server.
        assert!(
            guard_server_derived_url(base, "https://h.example/content/enforced/COURSE123//file.txt").is_err(),
            "duplicate slashes must be refused as ambiguous"
        );
        // Dot-segment aliases that normalizing servers collapse: servlet
        // path parameters (";..."), Windows-style trailing space/dot
        // trimming, and embedded NULs.
        assert!(
            guard_server_derived_url(base, "https://h.example/content/enforced/COURSE123/..%20/othercourse/file.txt").is_err(),
            "trailing-space dot segments must be refused"
        );
        assert!(
            guard_server_derived_url(base, "https://h.example/content/enforced/COURSE123/..%3B/othercourse/file.txt").is_err(),
            "path-parameter dot aliases must be refused"
        );
        assert!(
            guard_server_derived_url(base, "https://h.example/content/enforced/COURSE123/..%00/othercourse/file.txt").is_err(),
            "NUL-terminated dot segments must be refused"
        );
        assert!(
            guard_server_derived_url(base, "https://h.example/content/enforced/COURSE123/.../othercourse/file.txt").is_err(),
            "trailing-dot aliases of dot segments must be refused"
        );
        // In-collection targets still pass: deeper exact-case paths, explicit
        // default port, and percent-encoded ordinary characters.
        assert!(guard_server_derived_url(base, "https://h.example/content/enforced/COURSE123/notes/a.txt").is_ok());
        assert!(guard_server_derived_url(base, "https://h.example:443/content/enforced/COURSE123/a.txt").is_ok());
        assert!(guard_server_derived_url(base, "https://h.example/content/enforced/COURSE123/my%20file.txt").is_ok());
        // A legitimate name that merely ENDS in a dot or space is not a dot
        // segment: only spellings whose trailing-trimmed remainder collapses
        // to "", ".", or ".." can re-alias onto a dot segment.
        assert!(
            guard_server_derived_url(base, "https://h.example/content/enforced/COURSE123/report./a.txt").is_ok(),
            "trailing-dot file names must not be rejected as dot aliases"
        );
        assert!(
            guard_server_derived_url(base, "https://h.example/content/enforced/COURSE123/note%20file.txt").is_ok(),
            "in-segment spaces are ordinary characters"
        );
        // Encoded spellings that DECODE onto the collection name must not
        // match it: a server that treats `%33` literally routes the request
        // to a sibling collection, so the prefix comparison is byte-exact on
        // the raw segments.
        assert!(
            guard_server_derived_url(base, "https://h.example/content/enforced/COURSE12%33/file.txt").is_err(),
            "an encoded alias of the collection name must not match the collection"
        );
        // Double-encoded dot segments decode to `..` on a double-decoding
        // server and must be rejected before that can happen.
        assert!(
            guard_server_derived_url(base, "https://h.example/content/enforced/COURSE123/%252e%252e/othercourse/file.txt").is_err(),
            "double-encoded dot segments must be refused"
        );
        // Origin-root collection: any unambiguous non-root path is a strict
        // descendant, but the origin root itself is never an item target.
        assert!(guard_server_derived_url("https://h.example/", "https://h.example/any/path.txt").is_ok());
        assert!(
            guard_server_derived_url("https://h.example/", "https://h.example/").is_err(),
            "the origin root itself must not pass as an item target"
        );
        assert!(
            guard_server_derived_url("https://h.example/", "https://h.example/any/course%2F..%2Fother.txt").is_err(),
            "encoded-separator traversal is refused even for origin-root collections"
        );
    }

    #[test]
    fn test_create_unique_part_file_name_is_short_and_hidden() {
        // The temp sibling's name must not embed the target filename: a
        // long (valid) target name would push the temp past the filesystem
        // component limit (ENAMETOOLONG). Fixed prefix + fixed-width noise
        // only, hidden, and unique per call.
        let dir = std::env::temp_dir();
        let long_name = "x".repeat(200);
        let target = dir.join(&long_name);
        let (_f1, t1) = create_unique_part_file(&target).expect("temp creation must succeed");
        let (_f2, t2) = create_unique_part_file(&target).expect("second temp creation must succeed");
        let name1 = t1.file_name().and_then(|n| n.to_str()).unwrap();
        assert!(name1.starts_with('.'), "temp must be hidden: {name1}");
        assert!(
            name1.starts_with(".scs-part-"),
            "temp name is fixed-form, target-independent: {name1}"
        );
        assert!(
            name1.len() <= ".scs-part-".len() + 16,
            "temp name must be bounded by the noise width, got {name1}"
        );
        assert_ne!(t1, t2, "consecutive temps must not collide");
        let _ = std::fs::remove_file(&t1);
        let _ = std::fs::remove_file(&t2);
    }

    #[test]
    fn test_read_listing_body_decodes_bom_marked_utf16() {
        // WebDAV/XML bodies may be UTF-16 with a BOM; the bounded reader
        // must decode them under the same byte cap instead of failing.
        let xml = "<?xml version=\"1.0\"?><D:multistatus xmlns:D=\"DAV:\">\
                   <D:response><D:href>/course/a.txt</D:href></D:response>\
                   </D:multistatus>";
        let units: Vec<u16> = xml.encode_utf16().collect();
        let mut le = vec![0xFF, 0xFE];
        for u in &units {
            le.extend_from_slice(&u.to_le_bytes());
        }
        // Each read's ListingBody (and its in-flight reservation) is
        // released at the end of its own statement: shadowed bindings would
        // all live to scope end and stack three worst-case reservations.
        let body = read_listing_body(&mut std::io::Cursor::new(le))
            .expect("UTF-16LE with BOM must decode")
            .body;
        assert!(parse_propfind_xml(&body).is_ok());
        let mut be = vec![0xFE, 0xFF];
        for u in &units {
            be.extend_from_slice(&u.to_be_bytes());
        }
        let body = read_listing_body(&mut std::io::Cursor::new(be))
            .expect("UTF-16BE with BOM must decode")
            .body;
        assert!(parse_propfind_xml(&body).is_ok());
        // A UTF-8 BOM is accepted and stripped.
        let mut utf8bom = vec![0xEF, 0xBB, 0xBF];
        utf8bom.extend_from_slice(xml.as_bytes());
        let body = read_listing_body(&mut std::io::Cursor::new(utf8bom))
            .expect("UTF-8 BOM must be accepted")
            .body;
        assert!(parse_propfind_xml(&body).is_ok());
        // A truncated (odd-length) UTF-16 body fails closed.
        let mut odd = vec![0xFF, 0xFE, b'x'];
        let _ = &mut odd;
        assert!(read_listing_body(&mut std::io::Cursor::new(odd)).is_err());
    }

    #[test]
    fn test_parse_propfind_xml_rejects_oversized_hrefs() {
        // Item counts alone do not bound retained bytes: without a per-href
        // cap, one href can carry most of a listing body into per-item scan
        // state and blow past every aggregate budget.
        let href = format!("/course/{}.txt", "x".repeat(8192));
        let xml = format!(
            "<?xml version=\"1.0\"?><D:multistatus xmlns:D=\"DAV:\">\
             <D:response><D:href>{href}</D:href><D:propstat><D:prop>\
             <D:resourcetype/></D:prop><D:status>HTTP/1.1 200 OK</D:status>\
             </D:propstat></D:response></D:multistatus>"
        );
        let err = parse_propfind_xml(&xml)
            .expect_err("an oversized href must be refused outright");
        assert!(
            err.to_ascii_lowercase().contains("href"),
            "error must name the href limit, got: {err}"
        );
    }

    #[test]
    fn test_parse_propfind_xml_stops_at_item_cap() {
        // An over-cap listing is hostile, and truncated acceptance is a
        // silent partial listing: the parser itself must refuse it, so no
        // caller can be talked into treating the first cap+1 items as a
        // complete result (duplicates included — uniqueness is a caller
        // concern the parser cannot see).
        let mut xml =
            String::from("<?xml version=\"1.0\"?><D:multistatus xmlns:D=\"DAV:\">");
        for i in 0..(MAX_REMOTE_ITEMS + 500) {
            xml.push_str(&format!(
                "<D:response><D:href>/course/f{i}.txt</D:href>\
                 <D:propstat><D:prop><D:resourcetype/>\
                 <D:getcontentlength>1</D:getcontentlength></D:prop>\
                 <D:status>HTTP/1.1 200 OK</D:status></D:propstat></D:response>"
            ));
        }
        xml.push_str("</D:multistatus>");
        let err = parse_propfind_xml(&xml)
            .expect_err("an over-cap listing must be an outright parse error");
        assert!(
            err.to_ascii_lowercase().contains("item limit"),
            "error must name the item limit, got: {err}"
        );
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
        remove_part_file(&temp_file);
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

        // Case-variant usernames are distinct credentials: WebDAV usernames
        // are opaque, case-sensitive tokens, so "Alice" is not "alice" and
        // must neither read her listing nor share her key; an anonymous
        // request must not read it either.
        let case_variant_alice = Some("Alice");
        assert_ne!(cache_key(test_url, alice_user), cache_key(test_url, case_variant_alice));
        assert!(
            load_remote_cache(test_url, case_variant_alice).is_none(),
            "Alice (case variant) should not see alice's cached items"
        );
        assert!(
            load_remote_cache(test_url, None).is_none(),
            "an anonymous request should not see alice's cached items"
        );

        // Exact usernames: trailing whitespace distinguishes credentials,
        // and an ambiguous (empty/whitespace-only) username is neither
        // anonymous nor any account — cache entry points refuse it entirely
        // instead of conflating it with either.
        let spaced_alice = Some("alice ");
        assert_ne!(cache_key(test_url, alice_user), cache_key(test_url, spaced_alice));
        assert!(
            load_remote_cache(test_url, spaced_alice).is_none(),
            "a trailing-space username is a different credential"
        );
        assert!(
            load_remote_cache(test_url, Some("")).is_none(),
            "an empty username is not anonymous and must read nothing"
        );
        // Anonymous and empty-string are distinct credentials: alice's
        // stored entry answers neither (behavioral check — the comparator
        // itself is exercised through load).
        assert!(load_remote_cache(test_url, None).is_none());
        save_remote_cache(test_url, Some(""), &items);
        assert!(
            load_remote_cache(test_url, Some("")).is_none(),
            "ambiguous credentials are never cached"
        );
        assert!(
            load_remote_cache(test_url, None).is_none(),
            "an ambiguous credential must not poison the anonymous entry"
        );

        // Trailing slashes are part of the collection path: /files and
        // /files/ are distinct collections and must not share a key or an
        // entry (a shared entry steers the sync delete set at the wrong
        // collection).
        let slashed = "https://example.com/remote/files/";
        let unslashed = "https://example.com/remote/files";
        assert_ne!(cache_key(slashed, alice_user), cache_key(unslashed, alice_user));
        save_remote_cache(slashed, alice_user, &items);
        assert!(
            load_remote_cache(unslashed, alice_user).is_none(),
            "the unslashed collection must not read the slashed one's entry"
        );

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

        // Query-scoped collections are distinct listings: a different
        // share token must never read another token's entry, and the path
        // is identity exactly as configured — the trailing slash included —
        // so the unslashed spelling is a different collection.
        let query_url = "https://example.com/remote/files/?share=token1";
        save_remote_cache(query_url, alice_user, &items);
        assert!(
            load_remote_cache("https://example.com/remote/files?share=token2", alice_user).is_none(),
            "a different share token must not read another token's cached listing"
        );
        assert!(
            load_remote_cache("https://example.com/remote/files?share=token1", alice_user).is_none(),
            "the unslashed path is a different collection and must not read the slashed one's entry"
        );
        assert_eq!(
            load_remote_cache(query_url, alice_user)
                .expect("the exact configured spelling must hit its entry")
                .len(),
            items.len()
        );

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
    fn test_aggregate_remote_folders_credits_ancestors_and_includes_empty_dirs() {
        let base = "http://127.0.0.1:3923/docs/";
        let items = vec![
            WebdavItem::new("/docs/", true, 0),
            WebdavItem::new("/docs/root.txt", false, 5),
            WebdavItem::new("/docs/a/", true, 0),
            WebdavItem::new("/docs/a/b/", true, 0),
            WebdavItem::new("/docs/a/b/file.bin", false, 100),
            WebdavItem::new("/docs/a/empty/", true, 0),
        ];

        let folders = aggregate_remote_folders(base, &items);
        let by_path: Vec<(String, u64)> = folders
            .into_iter()
            .map(|f| (f.path, f.size))
            .collect();
        assert_eq!(
            by_path,
            vec![
                (String::new(), 105),
                ("a".to_string(), 100),
                ("a/b".to_string(), 100),
                ("a/empty".to_string(), 0),
            ]
        );
    }

    #[test]
    fn test_aggregate_remote_folders_decodes_percent_encoded_paths() {
        let base = "http://127.0.0.1:3923/docs/";
        let items = vec![
            WebdavItem::new("/docs/my%20dir/", true, 0),
            WebdavItem::new("/docs/my%20dir/file%20one.txt", false, 7),
        ];

        let folders = aggregate_remote_folders(base, &items);
        let by_path: Vec<(String, u64)> = folders
            .into_iter()
            .map(|f| (f.path, f.size))
            .collect();
        assert_eq!(
            by_path,
            vec![(String::new(), 7), ("my dir".to_string(), 7)]
        );
    }

    #[test]
    fn test_aggregate_remote_folders_sums_files_across_siblings() {
        let base = "http://127.0.0.1:3923/docs/";
        let items = vec![
            WebdavItem::new("/docs/x/", true, 0),
            WebdavItem::new("/docs/x/one.bin", false, 10),
            WebdavItem::new("/docs/y/", true, 0),
            WebdavItem::new("/docs/y/two.bin", false, 5),
            WebdavItem::new("/docs/y/three.bin", false, 2),
        ];

        let folders = aggregate_remote_folders(base, &items);
        let by_path: Vec<(String, u64)> = folders
            .into_iter()
            .map(|f| (f.path, f.size))
            .collect();
        assert_eq!(
            by_path,
            vec![
                (String::new(), 17),
                ("x".to_string(), 10),
                ("y".to_string(), 7),
            ]
        );
    }

    #[test]
    fn test_has_traversal_segments_rejects_literal_and_encoded_escapes() {
        for subdir in [
            "..",
            "a/../b",
            "..%2f",
            "..%2F",
            "%2e%2e",
            "%2E%2E",
            "..%2f..%2f",
            "a%2F..%2F..",
            "%2e%2e%2fetc",
            "a/..",
            "..\\..",
            "a%5Cb", // encoded backslash
            "a\\b",
            "..%00",           // NUL-truncated dot-dot
            "%252e%252e",      // double-encoded dot-dot
            "%252e%252e%252f", // double-encoded traversal
            "a%00b",           // raw NUL
            "a%01b",           // other control byte
        ] {
            assert!(
                has_traversal_segments(subdir),
                "subdir {subdir:?} must be rejected"
            );
        }
    }

    #[test]
    fn test_has_traversal_segments_accepts_plain_paths() {
        for subdir in [
            "",
            "content/enforced/1052175-dev_asteve18",
            "my%20dir/sub",
            "a/b/c",
            "a..b",
            "..hidden",
            "x/..y/z",
        ] {
            assert!(
                !has_traversal_segments(subdir),
                "subdir {subdir:?} must be accepted"
            );
        }
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

