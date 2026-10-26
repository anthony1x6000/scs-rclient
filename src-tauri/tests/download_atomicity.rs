//! A failed download must never destroy the pre-existing local file: the
//! transfer streams to a temporary sibling and replaces the destination only
//! after the body completed. The temp sibling must be unique and unguessable
//! (a predictable name can collide with another listed target and truncate
//! it), and a successful replace must preserve the prior file's permissions.

use scs_rclient_lib::webdav::execute_webdav_action_with_exclusions;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

/// The tests below mutate process-global env vars; this guard self-serializes
/// them so cargo test's parallel harness cannot interleave their env changes.
static ENV_GUARD: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Serves a PROPFIND listing of `(name, advertised_size)` entries and GETs
/// that either deliver the full advertised body or truncate it to 4 bytes and
/// drop the connection. Accept loop polls `stop`, so the thread terminates
/// when the action finishes and join() can never hang CI.
fn spawn_server(
    listing: Vec<(String, u64)>,
    full_bodies: bool,
    stop: Arc<AtomicBool>,
) -> std::io::Result<u16> {
    let listener = TcpListener::bind("127.0.0.1:0")?;
    listener.set_nonblocking(true)?;
    let port = listener.local_addr().unwrap().port();
    let stop_server = stop;
    std::thread::spawn(move || {
        while !stop_server.load(Ordering::SeqCst) {
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
                let responses: String = listing
                    .iter()
                    .map(|(name, size)| {
                        format!(
                            "<D:response><D:href>/course/{name}</D:href><D:propstat><D:prop>\
                             <D:resourcetype/><D:getcontentlength>{size}</D:getcontentlength>\
                             </D:prop><D:status>HTTP/1.1 200 OK</D:status></D:propstat></D:response>"
                        )
                    })
                    .collect();
                let body = format!(
                    "<?xml version=\"1.0\" encoding=\"utf-8\" ?>\
                     <D:multistatus xmlns:D=\"DAV:\">\
                     <D:response><D:href>/course/</D:href><D:propstat><D:prop>\
                     <D:resourcetype><D:collection/></D:resourcetype></D:prop>\
                     <D:status>HTTP/1.1 200 OK</D:status></D:propstat></D:response>\
                     {responses}</D:multistatus>"
                );
                let resp = format!(
                    "HTTP/1.1 207 Multi-Status\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                let _ = stream.write_all(resp.as_bytes());
            } else {
                // GET: honor the advertised size either fully or as a stall.
                let advertised: u64 = req
                    .split_whitespace()
                    .nth(1)
                    .and_then(|p| p.rsplit('/').next())
                    .and_then(|name| {
                        listing
                            .iter()
                            .find(|(n, _)| n == name)
                            .map(|(_, s)| *s)
                    })
                    .unwrap_or(4096);
                let headers = format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {advertised}\r\nConnection: close\r\n\r\n"
                );
                let _ = stream.write_all(headers.as_bytes());
                if full_bodies {
                    let mut remaining = advertised;
                    while remaining > 0 {
                        let n = remaining.min(4096) as usize;
                        let _ = stream.write_all(&vec![b'x'; n]);
                        remaining -= n as u64;
                    }
                } else {
                    let _ = stream.write_all(b"abcd");
                    let _ = stream.flush();
                    std::thread::sleep(Duration::from_millis(100));
                    drop(stream);
                }
            }
        }
    });
    Ok(port)
}

fn run_get_action(port: u16, local_dir: &std::path::Path, logs: &mut Vec<String>) {
    let base_url = format!("http://127.0.0.1:{port}/course/");
    let client = scs_rclient_lib::dav_client::Client::init("", "", &base_url).unwrap();
    let cancel = Arc::new(AtomicBool::new(false));
    let _ = execute_webdav_action_with_exclusions(
        &client,
        "get",
        &base_url,
        local_dir,
        &cancel,
        None,
        None,
        &[],
        |m: &str| logs.push(m.to_string()),
    );
}

#[test]
fn test_failed_download_preserves_existing_local_file() {
    let _env_guard = ENV_GUARD.lock().unwrap_or_else(|p| p.into_inner());
    std::env::set_var("ALLOW_INSECURE_WEBDAV", "1");
    std::env::set_var("WEBDAV_CACHE_DISABLED", "1");

    let stop = Arc::new(AtomicBool::new(false));
    let port = spawn_server(vec![("a.txt".to_string(), 4096)], false, stop.clone()).unwrap();
    let local_dir = std::env::temp_dir().join(format!("scs-atomic-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&local_dir);
    std::fs::create_dir_all(&local_dir).unwrap();
    let original = b"PRECIOUS LOCAL CONTENT v1";
    std::fs::write(local_dir.join("a.txt"), original).unwrap();
    let mut logs = Vec::new();
    run_get_action(port, &local_dir, &mut logs);
    stop.store(true, Ordering::SeqCst);

    let after = std::fs::read(local_dir.join("a.txt")).unwrap_or_default();
    let _ = std::fs::remove_dir_all(&local_dir);
    std::env::remove_var("ALLOW_INSECURE_WEBDAV");
    std::env::remove_var("WEBDAV_CACHE_DISABLED");

    assert_eq!(
        after, original,
        "a failed download must leave the pre-existing local file untouched (logs: {logs:?})"
    );
}

#[test]
fn test_temp_name_collision_cannot_destroy_sibling_file() {
    let _env_guard = ENV_GUARD.lock().unwrap_or_else(|p| p.into_inner());
    std::env::set_var("ALLOW_INSECURE_WEBDAV", "1");
    std::env::set_var("WEBDAV_CACHE_DISABLED", "1");

    // A hostile server lists the exact name the download would want to use
    // as its temp sibling (`a.scs-part-<pid>`); that sibling is itself a
    // real local file. Neither a failed nor a successful download may
    // truncate or delete it.
    let collide = format!("a.scs-part-{}", std::process::id());
    let stop = Arc::new(AtomicBool::new(false));
    let port = spawn_server(
        vec![("a.txt".to_string(), 4096), (collide.clone(), 4096)],
        false,
        stop.clone(),
    )
    .unwrap();
    let local_dir = std::env::temp_dir().join(format!("scs-atomic-collide-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&local_dir);
    std::fs::create_dir_all(&local_dir).unwrap();
    let original_a = b"PRECIOUS A";
    let original_b = b"PRECIOUS B";
    std::fs::write(local_dir.join("a.txt"), original_a).unwrap();
    std::fs::write(local_dir.join(&collide), original_b).unwrap();
    let mut logs = Vec::new();
    run_get_action(port, &local_dir, &mut logs);
    stop.store(true, Ordering::SeqCst);

    let after_a = std::fs::read(local_dir.join("a.txt")).unwrap_or_default();
    let after_b = std::fs::read(local_dir.join(&collide)).unwrap_or_default();
    let _ = std::fs::remove_dir_all(&local_dir);
    std::env::remove_var("ALLOW_INSECURE_WEBDAV");
    std::env::remove_var("WEBDAV_CACHE_DISABLED");

    assert_eq!(after_a, original_a, "failed download must not touch its own target (logs: {logs:?})");
    assert_eq!(
        after_b, original_b,
        "the download temp must never coincide with a real sibling file (logs: {logs:?})"
    );
}

#[cfg(unix)]
#[test]
fn test_successful_download_preserves_target_permissions() {
    use std::os::unix::fs::PermissionsExt;
    let _env_guard = ENV_GUARD.lock().unwrap_or_else(|p| p.into_inner());
    std::env::set_var("ALLOW_INSECURE_WEBDAV", "1");
    std::env::set_var("WEBDAV_CACHE_DISABLED", "1");

    let stop = Arc::new(AtomicBool::new(false));
    let port = spawn_server(vec![("a.txt".to_string(), 8)], true, stop.clone()).unwrap();
    let local_dir = std::env::temp_dir().join(format!("scs-atomic-mode-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&local_dir);
    std::fs::create_dir_all(&local_dir).unwrap();
    std::fs::write(local_dir.join("a.txt"), b"stale").unwrap();
    std::fs::set_permissions(local_dir.join("a.txt"), std::fs::Permissions::from_mode(0o600))
        .unwrap();
    let mut logs = Vec::new();
    run_get_action(port, &local_dir, &mut logs);
    stop.store(true, Ordering::SeqCst);

    let after = std::fs::read(local_dir.join("a.txt")).unwrap_or_default();
    let mode = std::fs::metadata(local_dir.join("a.txt"))
        .map(|m| m.permissions().mode())
        .unwrap_or(0);
    let _ = std::fs::remove_dir_all(&local_dir);
    std::env::remove_var("ALLOW_INSECURE_WEBDAV");
    std::env::remove_var("WEBDAV_CACHE_DISABLED");

    assert_eq!(after, b"xxxxxxxx", "the full body must land (logs: {logs:?})");
    assert_eq!(
        mode & 0o777,
        0o600,
        "replacing the target must keep the prior file's mode (logs: {logs:?})"
    );
}
