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
/// Body policy per GET: `None` truncates mid-stream and drops the
/// connection; `Some(n)` sends exactly `n` bytes as a cleanly terminated
/// body (which may legitimately differ from the advertised size in tests).
fn spawn_server(
    listing: Vec<(String, u64)>,
    full_bodies: Option<u64>,
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
                match full_bodies {
                    // Chunked and cleanly terminated: the client accepts the
                    // body without error regardless of any advertised
                    // length — exactly the hostile short-body shape.
                    Some(exact) => {
                        let headers =
                            "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n";
                        let _ = stream.write_all(headers.as_bytes());
                        let mut remaining = exact;
                        while remaining > 0 {
                            let n = remaining.min(4096) as usize;
                            let _ = stream.write_all(format!("{n:x}\r\n").as_bytes());
                            let _ = stream.write_all(&vec![b'x'; n]);
                            let _ = stream.write_all(b"\r\n");
                            remaining -= n as u64;
                        }
                        let _ = stream.write_all(b"0\r\n\r\n");
                    }
                    None => {
                        let headers = format!(
                            "HTTP/1.1 200 OK\r\nContent-Length: {advertised}\r\nConnection: close\r\n\r\n"
                        );
                        let _ = stream.write_all(headers.as_bytes());
                        let _ = stream.write_all(b"abcd");
                        let _ = stream.flush();
                        std::thread::sleep(Duration::from_millis(100));
                        drop(stream);
                    }
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
    let port = spawn_server(vec![("a.txt".to_string(), 4096)], None, stop.clone()).unwrap();
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

    // A hostile server lists names a download might want for its temp
    // sibling — the old predictable `a.scs-part-<pid>` shape and the new
    // hidden `.scs-part-<noise>` shape alike; each is itself a real local
    // file. Neither a failed nor a successful download may truncate or
    // delete any of them.
    let collide = format!("a.scs-part-{}", std::process::id());
    // A pre-existing file shaped exactly like the new hidden temp form:
    // create_new must skip this name, never truncate it.
    let hidden = ".scs-part-0000000000000000".to_string();
    let stop = Arc::new(AtomicBool::new(false));
    let port = spawn_server(
        vec![
            ("a.txt".to_string(), 4096),
            (collide.clone(), 4096),
            (hidden.clone(), 4096),
        ],
        None,
        stop.clone(),
    )
    .unwrap();
    let local_dir = std::env::temp_dir().join(format!("scs-atomic-collide-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&local_dir);
    std::fs::create_dir_all(&local_dir).unwrap();
    let original_a = b"PRECIOUS A";
    let original_b = b"PRECIOUS B";
    let original_c = b"PRECIOUS C";
    std::fs::write(local_dir.join("a.txt"), original_a).unwrap();
    std::fs::write(local_dir.join(&collide), original_b).unwrap();
    std::fs::write(local_dir.join(&hidden), original_c).unwrap();
    let mut logs = Vec::new();
    run_get_action(port, &local_dir, &mut logs);
    stop.store(true, Ordering::SeqCst);

    let after_a = std::fs::read(local_dir.join("a.txt")).unwrap_or_default();
    let after_b = std::fs::read(local_dir.join(&collide)).unwrap_or_default();
    let after_c = std::fs::read(local_dir.join(&hidden)).unwrap_or_default();
    let _ = std::fs::remove_dir_all(&local_dir);
    std::env::remove_var("ALLOW_INSECURE_WEBDAV");
    std::env::remove_var("WEBDAV_CACHE_DISABLED");

    assert_eq!(after_a, original_a, "failed download must not touch its own target (logs: {logs:?})");
    assert_eq!(
        after_b, original_b,
        "the download temp must never coincide with a real sibling file (logs: {logs:?})"
    );
    assert_eq!(
        after_c, original_c,
        "a file named like the hidden temp form must never be truncated (logs: {logs:?})"
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
    let port = spawn_server(vec![("a.txt".to_string(), 8)], Some(8), stop.clone()).unwrap();
    let local_dir = std::env::temp_dir().join(format!("scs-atomic-mode-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&local_dir);
    std::fs::create_dir_all(&local_dir).unwrap();
    std::fs::write(local_dir.join("a.txt"), b"stale").unwrap();
    // 0o644 is distinct from the temp's 0o600 creation mode, so the test
    // actually proves prior-mode copying rather than the default.
    std::fs::set_permissions(local_dir.join("a.txt"), std::fs::Permissions::from_mode(0o644))
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
        "the replace keeps the prior file's OWNER bits only — group/other are dropped with the inode (logs: {logs:?})"
    );
}

#[cfg(unix)]
#[test]
fn test_read_only_target_is_refused_not_replaced() {
    use std::os::unix::fs::PermissionsExt;
    let _env_guard = ENV_GUARD.lock().unwrap_or_else(|p| p.into_inner());
    std::env::set_var("ALLOW_INSECURE_WEBDAV", "1");
    std::env::set_var("WEBDAV_CACHE_DISABLED", "1");

    let stop = Arc::new(AtomicBool::new(false));
    let port = spawn_server(vec![("a.txt".to_string(), 8)], Some(8), stop.clone()).unwrap();
    let local_dir = std::env::temp_dir().join(format!("scs-atomic-ro-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&local_dir);
    std::fs::create_dir_all(&local_dir).unwrap();
    let original = b"READONLY CONTENT";
    std::fs::write(local_dir.join("a.txt"), original).unwrap();
    std::fs::set_permissions(local_dir.join("a.txt"), std::fs::Permissions::from_mode(0o444))
        .unwrap();
    // The guard the test exercises is ordinary Unix write permission: root
    // (or a CAP_FOWNER-equivalent) can open 0444 for writing everywhere, so
    // the assumption itself is probed and the test skipped where it cannot
    // hold.
    if let Ok(handle) = std::fs::OpenOptions::new()
        .write(true)
        .open(local_dir.join("a.txt"))
    {
        drop(handle);
        std::fs::set_permissions(
            local_dir.join("a.txt"),
            std::fs::Permissions::from_mode(0o644),
        )
        .unwrap();
        let _ = std::fs::remove_dir_all(&local_dir);
        std::env::remove_var("ALLOW_INSECURE_WEBDAV");
        std::env::remove_var("WEBDAV_CACHE_DISABLED");
        return; // running with DAC override: nothing to assert
    }
    let mut logs = Vec::new();
    run_get_action(port, &local_dir, &mut logs);
    stop.store(true, Ordering::SeqCst);

    let after = std::fs::read(local_dir.join("a.txt")).unwrap_or_default();
    let temps: Vec<_> = std::fs::read_dir(&local_dir)
        .map(|rd| {
            rd.filter_map(|e| e.ok())
                .filter(|e| e.file_name().to_string_lossy().starts_with(".scs-part-"))
                .collect()
        })
        .unwrap_or_default();
    std::fs::set_permissions(local_dir.join("a.txt"), std::fs::Permissions::from_mode(0o644))
        .unwrap();
    let _ = std::fs::remove_dir_all(&local_dir);
    std::env::remove_var("ALLOW_INSECURE_WEBDAV");
    std::env::remove_var("WEBDAV_CACHE_DISABLED");

    assert_eq!(
        after, original,
        "a read-only target must keep its content — the old truncate path failed loudly and the replace must too (logs: {logs:?})"
    );
    assert!(
        temps.is_empty(),
        "no partial temp may survive a refused download (logs: {logs:?})"
    );
    assert!(
        logs.iter().any(|l| l.to_ascii_lowercase().contains("permission") || l.to_ascii_lowercase().contains("not writable")),
        "the refusal must say why (logs: {logs:?})"
    );
}

#[cfg(unix)]
#[test]
fn test_non_writable_directory_fails_loudly() {
    use std::os::unix::fs::PermissionsExt;
    let _env_guard = ENV_GUARD.lock().unwrap_or_else(|p| p.into_inner());
    std::env::set_var("ALLOW_INSECURE_WEBDAV", "1");
    std::env::set_var("WEBDAV_CACHE_DISABLED", "1");

    let stop = Arc::new(AtomicBool::new(false));
    let port = spawn_server(vec![("a.txt".to_string(), 8)], Some(8), stop.clone()).unwrap();
    let local_dir = std::env::temp_dir().join(format!("scs-atomic-rodir-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&local_dir);
    std::fs::create_dir_all(&local_dir).unwrap();
    let original = b"PRIOR CONTENT";
    std::fs::write(local_dir.join("a.txt"), original).unwrap();
    // Read-only DIRECTORY: the old in-place truncate could still write the
    // (writable) file inside it; the atomic replace needs to create a temp
    // sibling there and must fail loudly instead, leaving the file intact.
    std::fs::set_permissions(&local_dir, std::fs::Permissions::from_mode(0o555)).unwrap();
    let mut logs = Vec::new();
    run_get_action(port, &local_dir, &mut logs);
    stop.store(true, Ordering::SeqCst);

    let after = std::fs::read(local_dir.join("a.txt")).unwrap_or_default();
    std::fs::set_permissions(&local_dir, std::fs::Permissions::from_mode(0o755)).unwrap();
    let _ = std::fs::remove_dir_all(&local_dir);
    std::env::remove_var("ALLOW_INSECURE_WEBDAV");
    std::env::remove_var("WEBDAV_CACHE_DISABLED");

    assert_eq!(
        after, original,
        "a read-only directory must keep the prior file — the replace refuses loudly (logs: {logs:?})"
    );
}

#[test]
fn test_short_clean_body_does_not_replace_prior_file() {
    let _env_guard = ENV_GUARD.lock().unwrap_or_else(|p| p.into_inner());
    std::env::set_var("ALLOW_INSECURE_WEBDAV", "1");
    std::env::set_var("WEBDAV_CACHE_DISABLED", "1");

    // The listing advertises 16 bytes; the GET returns a CLEANLY terminated
    // 8-byte body (as a hostile chunked response would). A truncated body
    // that ends without error must not replace the prior local file.
    let stop = Arc::new(AtomicBool::new(false));
    let port = spawn_server(vec![("a.txt".to_string(), 16)], Some(8), stop.clone()).unwrap();
    let local_dir = std::env::temp_dir().join(format!("scs-atomic-short-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&local_dir);
    std::fs::create_dir_all(&local_dir).unwrap();
    let original = b"PRIOR FULL CONTENT";
    std::fs::write(local_dir.join("a.txt"), original).unwrap();
    let mut logs = Vec::new();
    run_get_action(port, &local_dir, &mut logs);
    stop.store(true, Ordering::SeqCst);

    let after = std::fs::read(local_dir.join("a.txt")).unwrap_or_default();
    let temps: Vec<_> = std::fs::read_dir(&local_dir)
        .map(|rd| {
            rd.filter_map(|e| e.ok())
                .filter(|e| e.file_name().to_string_lossy().starts_with(".scs-part-"))
                .collect()
        })
        .unwrap_or_default();
    let _ = std::fs::remove_dir_all(&local_dir);
    std::env::remove_var("ALLOW_INSECURE_WEBDAV");
    std::env::remove_var("WEBDAV_CACHE_DISABLED");

    assert_eq!(
        after, original,
        "a cleanly short body must not replace the prior file (logs: {logs:?})"
    );
    assert!(temps.is_empty(), "no temp may survive a size-mismatch refusal (logs: {logs:?})");
    assert!(
        logs.iter().any(|l| l.contains("advertised")),
        "the refusal must name the advertised size (logs: {logs:?})"
    );
}
