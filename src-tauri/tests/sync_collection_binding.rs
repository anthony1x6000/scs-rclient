//! A server-advertised href outside the configured collection must never
//! become an authenticated request target during sync — no method, not just
//! DELETE.
//!
//! Mirrors the credential_leak.rs recording-server harness: the configured
//! WebDAV server (loopback) returns a PROPFIND listing that embeds an
//! out-of-collection href (`/othercourse/file.txt`). With an empty local
//! folder, sync's delete set would contain that file, and without the
//! collection guard the delete loop would issue
//! `DELETE /othercourse/file.txt` with the user's Basic credentials.

use scs_rclient_lib::webdav::execute_webdav_action_with_exclusions;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// Restores a process-wide env var to its previous value when dropped, so
/// this test's environment changes cannot leak into tests running after it.
struct EnvRestore(&'static str, Option<std::ffi::OsString>);

impl EnvRestore {
    fn set(key: &'static str, value: &str) -> Self {
        let prev = std::env::var_os(key);
        std::env::set_var(key, value);
        EnvRestore(key, prev)
    }
}

impl Drop for EnvRestore {
    fn drop(&mut self) {
        match &self.1 {
            Some(v) => std::env::set_var(self.0, v),
            None => std::env::remove_var(self.0),
        }
    }
}

fn start_recording_server() -> (u16, Arc<Mutex<Vec<String>>>, Arc<AtomicBool>, std::thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let requests: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let stop = Arc::new(AtomicBool::new(false));
    let requests_clone = requests.clone();
    let stop_clone = stop.clone();
    let handle = std::thread::spawn(move || {
        listener.set_nonblocking(true).unwrap();
        loop {
            if stop_clone.load(Ordering::SeqCst) {
                break;
            }
            match listener.accept() {
                Ok((mut stream, _)) => {
                    let _ = stream.set_read_timeout(Some(Duration::from_secs(3)));
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
                    let request = String::from_utf8_lossy(&buf).into_owned();
                    let first_line = request.lines().next().unwrap_or("").to_string();
                    requests_clone
                        .lock()
                        .unwrap_or_else(|p| p.into_inner())
                        .push(first_line.clone());
                    // PROPFIND answer: the collection itself, an in-collection
                    // control file (whose DELETE proves the delete loop ran),
                    // and the hostile out-of-collection href. DELETE gets a
                    // 204 either way so the loop would continue if the guard
                    // were absent.
                    let is_propfind = first_line.starts_with("PROPFIND");
                    let body = if is_propfind {
                        "<?xml version=\"1.0\" encoding=\"utf-8\" ?>\
<D:multistatus xmlns:D=\"DAV:\">\
<D:response><D:href>/mycourse/</D:href><D:propstat><D:prop><D:resourcetype><D:collection/></D:resourcetype></D:prop><D:status>HTTP/1.1 200 OK</D:status></D:propstat></D:response>\
<D:response><D:href>/mycourse/control.txt</D:href><D:propstat><D:prop><D:resourcetype/><D:getcontentlength>4</D:getcontentlength></D:prop><D:status>HTTP/1.1 200 OK</D:status></D:propstat></D:response>\
<D:response><D:href>/othercourse/file.txt</D:href><D:propstat><D:prop><D:resourcetype/><D:getcontentlength>6</D:getcontentlength></D:prop><D:status>HTTP/1.1 200 OK</D:status></D:propstat></D:response>\
</D:multistatus>"
                    } else {
                        ""
                    };
                    let status = if is_propfind { "207 Multi-Status" } else { "204 No Content" };
                    let resp = format!(
                        "HTTP/1.1 {status}\r\nContent-Type: application/xml; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                        body.len(),
                        body
                    );
                    let _ = stream.write_all(resp.as_bytes());
                }
                Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(10));
                }
                Err(_) => break,
            }
        }
    });
    (port, requests, stop, handle)
}

#[test]
fn test_sync_never_deletes_out_of_collection_hrefs() {
    let _allow_insecure = EnvRestore::set("ALLOW_INSECURE_WEBDAV", "1");
    let _cache_disabled = EnvRestore::set("WEBDAV_CACHE_DISABLED", "1");

    let (port, requests, stop, handle) = start_recording_server();
    let remote_url = format!("http://127.0.0.1:{port}/mycourse/");
    let local_dir = std::env::temp_dir().join(format!("scs-sync-guard-{}", std::process::id()));
    std::fs::create_dir_all(&local_dir).unwrap();
    let client = scs_rclient_lib::dav_client::Client::init("", "", &remote_url).unwrap();
    let cancel = std::sync::Arc::new(AtomicBool::new(false));
    let mut logs = Vec::new();

    let result = execute_webdav_action_with_exclusions(
        &client,
        "sync",
        &remote_url,
        &local_dir,
        &cancel,
        None,
        None,
        &[],
        None,
        |msg: &str| logs.push(msg.to_string()),
    );

    stop.store(true, Ordering::SeqCst);
    let _ = handle.join();
    let _ = std::fs::remove_dir_all(&local_dir);

    assert!(result.is_ok(), "sync itself should complete: {:?}", result.err());
    let recorded = requests.lock().unwrap_or_else(|p| p.into_inner()).clone();
    assert!(
        recorded.iter().any(|r| r.starts_with("PROPFIND")),
        "the scan must actually list the collection for this test to mean anything; recorded: {:?}",
        recorded
    );
    assert!(
        recorded
            .iter()
            .any(|r| r.starts_with("DELETE") && r.contains("/mycourse/control.txt")),
        "the in-collection control file must be deleted, proving the delete loop ran; recorded: {:?} (logs: {:?})",
        recorded,
        logs
    );
    let stray_requests: Vec<String> = recorded
        .iter()
        .filter(|r| r.contains("/othercourse/"))
        .cloned()
        .collect();
    assert!(
        stray_requests.is_empty(),
        "no authenticated request of any method may target the out-of-collection path; recorded: {:?} (logs: {:?})",
        recorded,
        logs
    );
}
