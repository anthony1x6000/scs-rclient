//! Downloading an existing but empty remote folder must still produce its local
//! counterpart: the app's local mirror is keyed by folder name, so a download
//! that finds nothing to copy but leaves no `<mount>/<SUBDIR>` folder behind is
//! indistinguishable from a download that never ran.
//!
//! The mock server is a WebDAV collection that exists and holds no files: every
//! PROPFIND returns a 207 for the root collection only, so Depth: infinity
//! reports no children and the Depth: 1 fallback scans one directory with zero
//! items.

use scs_rclient_lib::dav_client::Client;
use scs_rclient_lib::webdav::execute_webdav_action_with_exclusions;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

/// The tests mutate process-global env vars; this guard self-serializes them
/// so cargo test's parallel harness cannot interleave their env changes.
static ENV_GUARD: std::sync::Mutex<()> = std::sync::Mutex::new(());

const SUBDIR: &str = "SUBDIR";

/// Serves an empty collection at `/course/empty/`. Accept loops poll `stop`,
/// so the thread always terminates when the action finishes and join never
/// hangs CI.
fn spawn_empty_collection(stop: Arc<AtomicBool>) -> std::io::Result<u16> {
    let listener = TcpListener::bind("127.0.0.1:0")?;
    listener.set_nonblocking(true)?;
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        while !stop.load(Ordering::SeqCst) {
            let (mut stream, _) = match listener.accept() {
                Ok(accepted) => accepted,
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(10));
                    continue;
                }
                Err(_) => break,
            };
            let _ = stream.set_nonblocking(false);
            let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
            let mut buf = Vec::new();
            let mut chunk = [0u8; 4096];
            loop {
                match stream.read(&mut chunk) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        buf.extend_from_slice(&chunk[..n]);
                        if buf.windows(4).position(|w| w == b"\r\n\r\n").is_some() {
                            break;
                        }
                    }
                }
            }
            let req = String::from_utf8_lossy(&buf).to_string();
            if req.starts_with("PROPFIND") {
                let body = "<?xml version=\"1.0\" encoding=\"utf-8\" ?>\
                            <D:multistatus xmlns:D=\"DAV:\">\
                            <D:response><D:href>/course/empty/</D:href><D:propstat><D:prop>\
                            <D:resourcetype><D:collection/></D:resourcetype></D:prop>\
                            <D:status>HTTP/1.1 200 OK</D:status></D:propstat></D:response>\
                            </D:multistatus>";
                let resp = format!(
                    "HTTP/1.1 207 Multi-Status\r\nContent-Type: application/xml; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                let _ = stream.write_all(resp.as_bytes());
            } else {
                // Nothing here is downloadable; only the listing matters.
                let _ = stream.write_all(
                    b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                );
            }
        }
    });
    Ok(port)
}

fn run_get(port: u16, action: &str, local_dir: &std::path::Path) -> Vec<String> {
    let base_url = format!("http://127.0.0.1:{port}/course/empty/");
    let client = Client::init("", "", &base_url).unwrap();
    let cancel = Arc::new(AtomicBool::new(false));
    let mut logs = Vec::new();
    let _ = execute_webdav_action_with_exclusions(
        &client,
        action,
        &base_url,
        local_dir,
        &cancel,
        None,
        None,
        &[],
        None,
        |m: &str| logs.push(m.to_string()),
    );
    logs
}

/// A fresh `<temp>/<tag>/SUBDIR` layout: the mount directory exists (as in the
/// app) while the selected subdirectory does not.
fn mount_without_subdir(tag: &str) -> std::path::PathBuf {
    let mount = std::env::temp_dir().join(format!("scs-empty-get-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&mount);
    std::fs::create_dir_all(&mount).unwrap();
    mount
}

fn set_insecure_test_env() {
    std::env::set_var("ALLOW_INSECURE_WEBDAV", "1");
    std::env::set_var("WEBDAV_CACHE_DISABLED", "1");
}

fn clear_insecure_test_env() {
    std::env::remove_var("ALLOW_INSECURE_WEBDAV");
    std::env::remove_var("WEBDAV_CACHE_DISABLED");
}

#[test]
fn download_of_empty_remote_folder_creates_the_local_subdir() {
    let _env_guard = ENV_GUARD.lock().unwrap_or_else(|p| p.into_inner());
    set_insecure_test_env();

    let stop = Arc::new(AtomicBool::new(false));
    let port = spawn_empty_collection(stop.clone()).unwrap();
    let mount = mount_without_subdir("empty");
    let local_subdir = mount.join(SUBDIR);
    let logs = run_get(port, "get", &local_subdir);
    stop.store(true, Ordering::SeqCst);

    let is_dir = local_subdir.is_dir();
    let entries: Vec<_> = std::fs::read_dir(&local_subdir)
        .map(|dir| dir.filter_map(Result::ok).collect())
        .unwrap_or_default();
    let _ = std::fs::remove_dir_all(&mount);
    clear_insecure_test_env();

    assert!(
        is_dir,
        "an empty remote folder must create its empty local counterpart at {} (logs: {logs:?})",
        local_subdir.display()
    );
    assert!(
        entries.is_empty(),
        "the created folder must be empty (logs: {logs:?})"
    );
}

#[test]
fn dry_run_of_empty_remote_folder_touches_nothing() {
    let _env_guard = ENV_GUARD.lock().unwrap_or_else(|p| p.into_inner());
    set_insecure_test_env();

    let stop = Arc::new(AtomicBool::new(false));
    let port = spawn_empty_collection(stop.clone()).unwrap();
    let mount = mount_without_subdir("dry");
    let local_subdir = mount.join(SUBDIR);
    let logs = run_get(port, "get-dry", &local_subdir);
    stop.store(true, Ordering::SeqCst);

    let created = local_subdir.exists();
    let _ = std::fs::remove_dir_all(&mount);
    clear_insecure_test_env();

    assert!(
        !created,
        "a dry run must not write to disk, not even an empty folder (logs: {logs:?})"
    );
}
