//! A failed download must never destroy the pre-existing local file: the
//! transfer streams to a temporary sibling and replaces the destination only
//! after the body completed. Today File::create truncates the destination
//! before the first body byte and the failure path deletes it, so a hostile
//! server that advertises a differing size and then stalls the stream
//! destroys the user's prior local copy.

use scs_rclient_lib::webdav::execute_webdav_action_with_exclusions;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

#[test]
fn test_failed_download_preserves_existing_local_file() {
    std::env::set_var("ALLOW_INSECURE_WEBDAV", "1");
    std::env::set_var("WEBDAV_CACHE_DISABLED", "1");

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    // Nonblocking accept + stop flag: a blocking `incoming()` loop never
    // ends, so join()ing it below would hang the test (and CI) forever
    // once the action finishes and stops making connections.
    listener.set_nonblocking(true).unwrap();
    let port = listener.local_addr().unwrap().port();
    let stop = Arc::new(AtomicBool::new(false));
    let stop_clone = stop.clone();
    let handle = std::thread::spawn(move || {
        while !stop_clone.load(Ordering::SeqCst) {
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
                let body = "<?xml version=\"1.0\" encoding=\"utf-8\" ?><D:multistatus xmlns:D=\"DAV:\"><D:response><D:href>/course/</D:href><D:propstat><D:prop><D:resourcetype><D:collection/></D:resourcetype></D:prop><D:status>HTTP/1.1 200 OK</D:status></D:propstat></D:response><D:response><D:href>/course/a.txt</D:href><D:propstat><D:prop><D:resourcetype/><D:getcontentlength>4096</D:getcontentlength><D:getlastmodified>Mon, 01 Jan 2035 00:00:00 GMT</D:getlastmodified></D:prop><D:status>HTTP/1.1 200 OK</D:status></D:propstat></D:response></D:multistatus>";
                let resp = format!(
                    "HTTP/1.1 207 Multi-Status\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                let _ = stream.write_all(resp.as_bytes());
            } else {
                // GET: promise 4096 bytes, deliver 4, then drop the connection.
                let _ = stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 4096\r\nConnection: close\r\n\r\nabcd");
                let _ = stream.flush();
                std::thread::sleep(Duration::from_millis(100));
                drop(stream);
            }
        }
    });

    let base_url = format!("http://127.0.0.1:{port}/course/");
    let client = scs_rclient_lib::dav_client::Client::init("", "", &base_url).unwrap();
    let local_dir = std::env::temp_dir().join(format!("scs-atomic-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&local_dir);
    std::fs::create_dir_all(&local_dir).unwrap();
    let original = b"PRECIOUS LOCAL CONTENT v1";
    std::fs::write(local_dir.join("a.txt"), original).unwrap();
    let cancel = Arc::new(AtomicBool::new(false));
    let mut logs = Vec::new();
    let _ = execute_webdav_action_with_exclusions(
        &client,
        "get",
        &base_url,
        &local_dir,
        &cancel,
        None,
        None,
        &[],
        |m: &str| logs.push(m.to_string()),
    );
    stop.store(true, Ordering::SeqCst);
    let _ = handle.join();
    let after = std::fs::read(local_dir.join("a.txt")).unwrap_or_default();
    let _ = std::fs::remove_dir_all(&local_dir);
    std::env::remove_var("ALLOW_INSECURE_WEBDAV");

    assert_eq!(
        after, original,
        "a failed download must leave the pre-existing local file untouched (logs: {logs:?})"
    );
}
