//! A slow-drip GET body must not hang the download loop, and cancel must
//! interrupt an in-flight transfer: io::copy observes neither the cancel flag
//! nor any whole-body deadline, and each blocking read gets a fresh 30s window
//! from reqwest, so a 1-byte-per-29s drip otherwise parks the action forever.

use scs_rclient_lib::webdav::execute_webdav_action_with_exclusions;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

fn start_drip_server() -> (u16, Arc<AtomicBool>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let stop = Arc::new(AtomicBool::new(false));
    let stop_clone = stop.clone();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            if stop_clone.load(Ordering::SeqCst) {
                break;
            }
            let mut stream = stream.unwrap();
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
                let body = "<?xml version=\"1.0\" encoding=\"utf-8\" ?><D:multistatus xmlns:D=\"DAV:\"><D:response><D:href>/course/</D:href><D:propstat><D:prop><D:resourcetype><D:collection/></D:resourcetype></D:prop><D:status>HTTP/1.1 200 OK</D:status></D:propstat></D:response><D:response><D:href>/course/drip.bin</D:href><D:propstat><D:prop><D:resourcetype/><D:getcontentlength>100000</D:getcontentlength></D:prop><D:status>HTTP/1.1 200 OK</D:status></D:propstat></D:response></D:multistatus>";
                let resp = format!(
                    "HTTP/1.1 207 Multi-Status\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                let _ = stream.write_all(resp.as_bytes());
            } else {
                // GET: drip one byte every 100ms, up to 30 seconds, then stop.
                let headers = "HTTP/1.1 200 OK\r\nContent-Length: 100000\r\nConnection: close\r\n\r\n";
                let _ = stream.write_all(headers.as_bytes());
                let start = Instant::now();
                while start.elapsed() < Duration::from_secs(30) && !stop_clone.load(Ordering::SeqCst) {
                    let _ = stream.write_all(b"x");
                    let _ = stream.flush();
                    std::thread::sleep(Duration::from_millis(100));
                }
                break;
            }
        }
    });
    (port, stop)
}

#[test]
fn test_cancel_interrupts_dripping_download_body() {
    std::env::set_var("ALLOW_INSECURE_WEBDAV", "1");
    std::env::set_var("WEBDAV_CACHE_DISABLED", "1");

    let (port, stop) = start_drip_server();
    let base_url = format!("http://127.0.0.1:{port}/course/");
    let client = scs_rclient_lib::dav_client::Client::init("", "", &base_url).unwrap();
    let local_dir = std::env::temp_dir().join(format!("scs-drip-{}", std::process::id()));
    std::fs::create_dir_all(&local_dir).unwrap();
    let cancel = Arc::new(AtomicBool::new(false));
    let cancel_for_thread = cancel.clone();
    let started = Arc::new(Mutex::new(false));
    let started_for_thread = started.clone();
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_secs(2));
        *started_for_thread.lock().unwrap() = true;
        cancel_for_thread.store(true, Ordering::SeqCst);
    });

    let mut logs = Vec::new();
    let result = execute_webdav_action_with_exclusions(
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
    let elapsed_ok = result.is_ok();
    stop.store(true, Ordering::SeqCst);
    let _ = std::fs::remove_dir_all(&local_dir);
    std::env::remove_var("ALLOW_INSECURE_WEBDAV");

    assert!(elapsed_ok, "action must terminate after cancel: {result:?}");
    assert!(
        logs.iter().any(|l| l.contains("canceled") || l.contains("canceled by user")),
        "expected a cancellation log line, got: {logs:?}"
    );
}

#[test]
fn test_body_deadline_stops_dripping_download() {
    std::env::set_var("ALLOW_INSECURE_WEBDAV", "1");
    std::env::set_var("WEBDAV_CACHE_DISABLED", "1");
    // 1s whole-body budget against a body that drips forever otherwise: the
    // drip makes steady progress, so only the deadline can end the transfer.
    std::env::set_var("WEBDAV_DOWNLOAD_BODY_TIMEOUT_SECS", "1");

    let (port, stop) = start_drip_server();
    let base_url = format!("http://127.0.0.1:{port}/course/");
    let client = scs_rclient_lib::dav_client::Client::init("", "", &base_url).unwrap();
    let local_dir = std::env::temp_dir().join(format!("scs-deadline-{}", std::process::id()));
    std::fs::create_dir_all(&local_dir).unwrap();
    let cancel = Arc::new(AtomicBool::new(false));

    let mut logs = Vec::new();
    let started = Instant::now();
    let result = execute_webdav_action_with_exclusions(
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
    let elapsed = started.elapsed();
    stop.store(true, Ordering::SeqCst);

    let action_ok = result.is_ok();
    let partial_left = local_dir.join("drip.bin").exists();
    let _ = std::fs::remove_dir_all(&local_dir);
    std::env::remove_var("ALLOW_INSECURE_WEBDAV");
    std::env::remove_var("WEBDAV_DOWNLOAD_BODY_TIMEOUT_SECS");

    // The action completes; the doomed file is skipped with a deadline error.
    assert!(action_ok, "action must terminate on the body deadline: {result:?}");
    assert!(
        elapsed < Duration::from_secs(20),
        "the deadline must stop the 30s drip early, took {elapsed:?}"
    );
    assert!(
        logs.iter().any(|l| l.contains("exceeded the 1s time limit")),
        "expected the deadline error in the log, got: {logs:?}"
    );
    assert!(!partial_left, "the partial download must be cleaned up");
}
