//! Proves credentials cannot leak: not to the wrong host, not into logs.
//!
//! Simulates the local-network attacker model from the security audit:
//! - Server A is the WebDAV server the user configured (the only principal
//!   allowed to see the Basic-auth header).
//! - Server B is an attacker listener. The listing from Server A contains a
//!   malicious absolute href pointing at Server B. Before the origin check,
//!   the client would have sent the authenticated GET — password included —
//!   straight to the attacker.
//!
//! The test asserts:
//! 1. Server A receives the Authorization header (credentials DO work).
//! 2. Server B receives ZERO requests (origin check blocks the leak).
//! 3. No emitted log line contains the raw password or its base64 form.

use scs_rclient_lib::dav_client::Client;
use scs_rclient_lib::webdav::execute_webdav_action_with_exclusions;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

const TEST_USER: &str = "leaktest-user";
const TEST_PASS: &str = "leaktest-password-DO-NOT-EMIT";

/// Minimal base64 encoder (standard alphabet, no padding edge cases for our
/// test inputs) so the test can predict the exact Authorization header value.
fn base64(input: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in input.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        out.push(TABLE[(b[0] >> 2) as usize] as char);
        out.push(TABLE[(((b[0] & 0x03) << 4) | (b[1] >> 4)) as usize] as char);
        out.push(if chunk.len() > 1 {
            TABLE[(((b[1] & 0x0f) << 2) | (b[2] >> 6)) as usize] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            TABLE[(b[2] & 0x3f) as usize] as char
        } else {
            '='
        });
    }
    out
}

/// Serves one PROPFIND response whose listing embeds a cross-host href, then
/// records every received request (method, path, Authorization header).
fn start_recording_server(
    propfind_body: String,
) -> (u16, Arc<Mutex<Vec<String>>>, Arc<AtomicBool>, std::thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let requests: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let stop = Arc::new(AtomicBool::new(false));
    let body = format!(
        "HTTP/1.1 207 Multi-Status\r\nContent-Type: application/xml; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        propfind_body.len(),
        propfind_body
    );
    let requests_clone = requests.clone();
    let stop_clone = stop.clone();
    let handle = std::thread::spawn(move || {
        listener
            .set_nonblocking(true)
            .expect("nonblocking listener");
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
                    let auth = request
                        .lines()
                        .find(|l| l.to_ascii_lowercase().starts_with("authorization:"))
                        .unwrap_or("(no authorization header)")
                        .to_string();
                    let first_line = request.lines().next().unwrap_or("").to_string();
                    requests_clone
                        .lock()
                        .unwrap_or_else(|p| p.into_inner())
                        .push(format!("{first_line} | {auth}"));
                    let _ = stream.write_all(body.as_bytes());
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
fn test_credentials_reach_only_validated_origin_never_attacker() {
    std::env::set_var("ALLOW_INSECURE_WEBDAV", "1");
    std::env::set_var("WEBDAV_CACHE_DISABLED", "1");

    // Attacker server (B): any request it receives is a credential leak.
    let (attacker_port, attacker_requests, attacker_stop, attacker_thread) =
        start_recording_server(
            "HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nhi".to_string(),
        );

    // Configured WebDAV server (A): returns a listing whose file href is an
    // absolute URL to the attacker — exactly the audit's HIGH finding.
    let propfind = format!(
        r#"<?xml version="1.0" encoding="utf-8"?>
<D:multistatus xmlns:D="DAV:">
<D:response><D:href>/course/</D:href><D:propstat><D:prop><D:resourcetype><D:collection/></D:resourcetype></D:prop><D:status>HTTP/1.1 200 OK</D:status></D:propstat></D:response>
<D:response><D:href>http://127.0.0.1:{attacker_port}/stolen-credentials.txt</D:href><D:propstat><D:prop><D:resourcetype/><D:getcontentlength>10</D:getcontentlength></D:prop><D:status>HTTP/1.1 200 OK</D:status></D:propstat></D:response>
</D:multistatus>"#
    );
    let (server_port, server_requests, server_stop, server_thread) =
        start_recording_server(propfind);

    let base_url = format!("http://127.0.0.1:{server_port}/course/");
    // local_dir does not exist: the local scan returns nothing, and the
    // malicious item is skipped before any file is created.
    let local_dir = std::path::Path::new("/nonexistent-leak-test-dir");
    let client = Client::init(TEST_USER, TEST_PASS, &base_url).expect("client init");
    let cancel_flag = AtomicBool::new(false);

    let mut log_lines: Vec<String> = Vec::new();
    let result = {
        let mut log = |msg: &str| log_lines.push(msg.to_string());
        execute_webdav_action_with_exclusions(
            &client,
            "get",
            &base_url,
            local_dir,
            &cancel_flag,
            None,
            Some(TEST_USER),
            &[],
            None,
            &mut log,
        )
    };

    // Give the servers a moment to finish recording, then stop them.
    std::thread::sleep(Duration::from_millis(200));
    server_stop.store(true, Ordering::SeqCst);
    attacker_stop.store(true, Ordering::SeqCst);
    let _ = server_thread.join();
    let _ = attacker_thread.join();

    // The action itself must succeed (the skip is logged, not fatal).
    assert!(result.is_ok(), "get action failed: {:?}", result.err());

    let server_reqs = server_requests.lock().unwrap_or_else(|p| p.into_inner()).clone();
    let attacker_reqs = attacker_requests
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .clone();

    // 1. The configured server DID receive the authenticated listing request.
    let expected_auth = base64(format!("{TEST_USER}:{TEST_PASS}").as_bytes());
    assert!(
        server_reqs.iter().any(|r| r.to_ascii_lowercase().contains(&expected_auth.to_ascii_lowercase())),
        "configured server never saw the expected Authorization header; got: {server_reqs:?}"
    );

    // 2. The attacker received ZERO requests — the origin check held.
    assert!(
        attacker_reqs.is_empty(),
        "CREDENTIAL LEAK: attacker server received {} request(s): {attacker_reqs:?}",
        attacker_reqs.len()
    );

    // 3. The skip was logged with the origin reason.
    let all_logs = log_lines.join("");
    assert!(
        all_logs.contains("outside the configured remote origin"),
        "expected origin-skip log line, got: {all_logs}"
    );

    // 4. No log line contains the raw password or its base64 form.
    let encoded = base64(format!("{TEST_USER}:{TEST_PASS}").as_bytes());
    assert!(
        !all_logs.contains(TEST_PASS),
        "PASSWORD LEAK INTO LOGS: raw password appears in log output"
    );
    assert!(
        !all_logs.contains(&encoded),
        "PASSWORD LEAK INTO LOGS: base64 credential appears in log output"
    );
}
