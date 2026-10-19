//! Verifies that the Settings "Threads" value resizes a worker pool while the
//! operation is already running, without cancelling and restarting it.

use scs_rclient_lib::webdav::{
    execute_webdav_action_with_options, list_remote_recursive, set_scan_concurrency,
    DEFAULT_SCAN_CONCURRENCY,
};
use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const CHILDREN: usize = 16;
const REQUEST_DELAY: Duration = Duration::from_millis(200);

/// Both tests mutate the process-wide live limit, so they must not overlap.
static SERIAL: Mutex<()> = Mutex::new(());

/// The production URL guard blocks loopback unless explicitly allowed for tests.
fn init_env() {
    std::env::set_var("ALLOW_INSECURE_WEBDAV", "1");
    std::env::set_var("WEBDAV_CACHE_DISABLED", "1");
}

/// Reads one HTTP request fully, including a possible request body.
fn read_request(stream: &mut TcpStream) -> Option<String> {
    let _ = stream.set_read_timeout(Some(Duration::from_secs(3)));
    let mut buf = Vec::new();
    let mut chunk = [0u8; 4096];
    loop {
        let n = stream.read(&mut chunk).ok()?;
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&chunk[..n]);
        if let Some(head_end) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            let head = String::from_utf8_lossy(&buf[..head_end]).to_ascii_lowercase();
            let body_len = head
                .lines()
                .find_map(|line| line.strip_prefix("content-length:"))
                .and_then(|value| value.trim().parse::<usize>().ok())
                .unwrap_or(0);
            if buf.len() >= head_end + 4 + body_len {
                break;
            }
        }
    }
    Some(String::from_utf8_lossy(&buf).into_owned())
}

fn multistatus(xml: &str) -> Vec<u8> {
    format!(
        "HTTP/1.1 207 Multi-Status\r\nContent-Type: application/xml; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        xml.len(),
        xml
    )
    .into_bytes()
}

#[test]
fn test_scan_concurrency_changes_mid_operation() {
    let _serial = SERIAL.lock().unwrap_or_else(|p| p.into_inner());
    init_env();

    let mut responses: HashMap<String, Vec<u8>> = HashMap::new();
    let mut root_xml =
        String::from(r#"<?xml version="1.0" encoding="utf-8"?><D:multistatus xmlns:D="DAV:">"#);
    root_xml.push_str(
        r#"<D:response><D:href>/adapt-course/</D:href><D:propstat><D:prop><D:resourcetype><D:collection/></D:resourcetype></D:prop><D:status>HTTP/1.1 200 OK</D:status></D:propstat></D:response>"#,
    );
    for i in 0..CHILDREN {
        root_xml.push_str(&format!(
            r#"<D:response><D:href>/adapt-course/dir{:02}/</D:href><D:propstat><D:prop><D:resourcetype><D:collection/></D:resourcetype></D:prop><D:status>HTTP/1.1 200 OK</D:status></D:propstat></D:response>"#,
            i
        ));
        let child_xml = format!(
            r#"<?xml version="1.0" encoding="utf-8"?><D:multistatus xmlns:D="DAV:"><D:response><D:href>/adapt-course/dir{:02}/</D:href><D:propstat><D:prop><D:resourcetype><D:collection/></D:resourcetype></D:prop><D:status>HTTP/1.1 200 OK</D:status></D:propstat></D:response><D:response><D:href>/adapt-course/dir{:02}/file.txt</D:href><D:propstat><D:prop><D:resourcetype/><D:getcontentlength>10</D:getcontentlength></D:prop><D:status>HTTP/1.1 200 OK</D:status></D:propstat></D:response></D:multistatus>"#,
            i, i
        );
        responses.insert(
            format!("/adapt-course/dir{:02}", i),
            multistatus(&child_xml),
        );
    }
    root_xml.push_str("</D:multistatus>");
    responses.insert("/adapt-course".to_string(), multistatus(&root_xml));

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let responses = Arc::new(responses);
    let stop = Arc::new(AtomicBool::new(false));
    let in_flight = Arc::new(AtomicUsize::new(0));
    let peak = Arc::new(AtomicUsize::new(0));

    let server_stop = stop.clone();
    let server_in_flight = in_flight.clone();
    let server_peak = peak.clone();
    let server_responses = responses.clone();
    let server = std::thread::spawn(move || {
        for conn in listener.incoming() {
            if server_stop.load(Ordering::SeqCst) {
                break;
            }
            let Ok(mut stream) = conn else { break };
            let responses = server_responses.clone();
            let in_flight = server_in_flight.clone();
            let peak = server_peak.clone();
            std::thread::spawn(move || {
                let mut buf = [0u8; 4096];
                let Ok(n) = stream.read(&mut buf) else {
                    return;
                };
                if n == 0 {
                    return;
                }
                let request = String::from_utf8_lossy(&buf[..n]);
                // Force the Depth: 1 fallback immediately; only that phase has real work.
                if request.to_ascii_lowercase().contains("depth: infinity") {
                    let _ = stream.write_all(
                        b"HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                    );
                    return;
                }
                let path = request
                    .lines()
                    .next()
                    .and_then(|line| line.split_whitespace().nth(1))
                    .unwrap_or("/");
                let key = path.trim_end_matches('/').to_ascii_lowercase();
                // Count before the delay and clear before responding, so the peak only
                // reflects requests that are genuinely overlapped.
                let active = in_flight.fetch_add(1, Ordering::SeqCst) + 1;
                peak.fetch_max(active, Ordering::SeqCst);
                std::thread::sleep(REQUEST_DELAY);
                in_flight.fetch_sub(1, Ordering::SeqCst);
                match responses.get(&key) {
                    Some(body) => {
                        let _ = stream.write_all(body);
                    }
                    None => {
                        let _ = stream.write_all(
                            b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                        );
                    }
                }
            });
        }
    });

    set_scan_concurrency(1);
    let base_url = format!("http://127.0.0.1:{}/adapt-course/", port);
    let cancel_flag = AtomicBool::new(false);
    let client = rustydav::client::Client::init("testuser", "testpass");

    let items = std::thread::scope(|scope| {
        let scan = scope.spawn(|| list_remote_recursive(&client, &base_url, &cancel_flag));

        // Wait for the scan to reach the network, then raise the limit while the
        // operation is still in flight.
        let deadline = Instant::now() + Duration::from_secs(5);
        while peak.load(Ordering::SeqCst) == 0 {
            assert!(Instant::now() < deadline, "scan never issued a request");
            std::thread::sleep(Duration::from_millis(10));
        }
        set_scan_concurrency(4);

        let deadline = Instant::now() + Duration::from_secs(5);
        while peak.load(Ordering::SeqCst) < 4 {
            assert!(
                Instant::now() < deadline,
                "worker pool did not grow after the live limit changed (peak {})",
                peak.load(Ordering::SeqCst)
            );
            std::thread::sleep(Duration::from_millis(10));
        }

        scan.join().unwrap().unwrap()
    });

    let dirs = items.iter().filter(|item| item.is_dir).count();
    let files = items.iter().filter(|item| !item.is_dir).count();
    assert_eq!(dirs, CHILDREN, "all subdirectories must still be scanned");
    assert_eq!(files, CHILDREN, "all files must still be discovered");

    stop.store(true, Ordering::SeqCst);
    let _ = TcpStream::connect(format!("127.0.0.1:{}", port));
    let _ = server.join();

    set_scan_concurrency(DEFAULT_SCAN_CONCURRENCY);
}

#[test]
fn test_upload_concurrency_changes_mid_operation() {
    let _serial = SERIAL.lock().unwrap_or_else(|p| p.into_inner());
    init_env();

    const FILES: usize = 12;

    let local_dir = std::env::temp_dir().join(format!("scs_live_conc_put_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&local_dir);
    std::fs::create_dir_all(&local_dir).unwrap();
    for i in 0..FILES {
        std::fs::write(local_dir.join(format!("file{:02}.txt", i)), b"payload").unwrap();
    }

    // The remote scan sees an empty collection, so every local file is uploaded.
    let root_xml = r#"<?xml version="1.0" encoding="utf-8"?><D:multistatus xmlns:D="DAV:"><D:response><D:href>/put-course/</D:href><D:propstat><D:prop><D:resourcetype><D:collection/></D:resourcetype></D:prop><D:status>HTTP/1.1 200 OK</D:status></D:propstat></D:response></D:multistatus>"#;
    let root_response = multistatus(root_xml);

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let stop = Arc::new(AtomicBool::new(false));
    let in_flight = Arc::new(AtomicUsize::new(0));
    let peak = Arc::new(AtomicUsize::new(0));

    let server_stop = stop.clone();
    let server_in_flight = in_flight.clone();
    let server_peak = peak.clone();
    let server = std::thread::spawn(move || {
        for conn in listener.incoming() {
            if server_stop.load(Ordering::SeqCst) {
                break;
            }
            let Ok(mut stream) = conn else { break };
            let root_response = root_response.clone();
            let in_flight = server_in_flight.clone();
            let peak = server_peak.clone();
            std::thread::spawn(move || {
                let Some(request) = read_request(&mut stream) else {
                    return;
                };
                if request.to_ascii_lowercase().contains("depth: infinity") {
                    let _ = stream.write_all(
                        b"HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                    );
                    return;
                }
                let method = request
                    .lines()
                    .next()
                    .and_then(|line| line.split_whitespace().next())
                    .unwrap_or("");
                match method {
                    "PROPFIND" => {
                        let _ = stream.write_all(&root_response);
                    }
                    "MKCOL" => {
                        let _ = stream.write_all(
                            b"HTTP/1.1 201 Created\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                        );
                    }
                    "PUT" => {
                        let active = in_flight.fetch_add(1, Ordering::SeqCst) + 1;
                        peak.fetch_max(active, Ordering::SeqCst);
                        std::thread::sleep(REQUEST_DELAY);
                        in_flight.fetch_sub(1, Ordering::SeqCst);
                        let _ = stream.write_all(
                            b"HTTP/1.1 201 Created\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                        );
                    }
                    _ => {
                        let _ = stream.write_all(
                            b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                        );
                    }
                }
            });
        }
    });

    set_scan_concurrency(1);
    let base_url = format!("http://127.0.0.1:{}/put-course/", port);
    let cancel_flag = AtomicBool::new(false);
    let client = rustydav::client::Client::init("testuser", "testpass");

    let result = std::thread::scope(|scope| {
        let action = scope.spawn(|| {
            execute_webdav_action_with_options(
                &client,
                "put",
                &base_url,
                &local_dir,
                &cancel_flag,
                None,
                None,
                |_| {},
            )
        });

        let deadline = Instant::now() + Duration::from_secs(5);
        while peak.load(Ordering::SeqCst) == 0 {
            assert!(Instant::now() < deadline, "upload never started");
            std::thread::sleep(Duration::from_millis(10));
        }
        set_scan_concurrency(4);

        let deadline = Instant::now() + Duration::from_secs(5);
        while peak.load(Ordering::SeqCst) < 4 {
            assert!(
                Instant::now() < deadline,
                "upload pool did not grow after the live limit changed (peak {})",
                peak.load(Ordering::SeqCst)
            );
            std::thread::sleep(Duration::from_millis(10));
        }

        action.join().unwrap()
    });

    assert!(
        result.is_ok(),
        "put with a live thread change failed: {:?}",
        result
    );

    stop.store(true, Ordering::SeqCst);
    let _ = TcpStream::connect(format!("127.0.0.1:{}", port));
    let _ = server.join();
    let _ = std::fs::remove_dir_all(&local_dir);

    set_scan_concurrency(DEFAULT_SCAN_CONCURRENCY);
}
