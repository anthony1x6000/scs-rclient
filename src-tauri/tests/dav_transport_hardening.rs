//! Proves the two transport residuals from the security audit are closed.
//!
//! Attacker model: a malicious or compromised WebDAV server answers a request
//! with `302 Location: http://attacker/...`. With the old `rustydav` 0.1.3
//! transport (a stock `reqwest::blocking::Client::new()`), reqwest followed
//! the redirect and re-sent the request — Basic-auth header included — to
//! whatever host the server named, and returned the final response as if the
//! configured server produced it.
//!
//! The in-repo `dav_client` builds its transport with
//! `redirect::Policy::none()`, so the 3xx comes straight back to the caller
//! and no second request is ever made. This test asserts exactly that:
//! the configured server sees the authenticated request, the redirect
//! target sees nothing, and the caller observes the raw 302 status.

use scs_rclient_lib::dav_client::Client;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

const TEST_USER: &str = "redirect-test-user";
const TEST_PASS: &str = "redirect-test-pass";

/// Serves one fixed response for every request, recording each first line
/// and Authorization header it receives.
fn start_fixed_response_server(
    response: String,
) -> (
    u16,
    Arc<Mutex<Vec<String>>>,
    Arc<AtomicBool>,
    std::thread::JoinHandle<()>,
) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let requests: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let stop = Arc::new(AtomicBool::new(false));
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
                    let _ = stream.write_all(response.as_bytes());
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
fn redirects_are_never_followed_and_credentials_stay_home() {
    std::env::set_var("ALLOW_INSECURE_WEBDAV", "1");

    // Attacker server: any request it receives is a credential exfiltration.
    let (attacker_port, attacker_requests, attacker_stop, attacker_thread) =
        start_fixed_response_server("HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nhi".to_string());

    // Configured WebDAV server: answers every request with a redirect to the
    // attacker, the exact 3xx exfiltration path from the audit.
    let redirect = format!(
        "HTTP/1.1 302 Found\r\nLocation: http://127.0.0.1:{attacker_port}/lure\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
    );
    let (server_port, server_requests, server_stop, server_thread) =
        start_fixed_response_server(redirect);

    let base_url = format!("http://127.0.0.1:{server_port}/course/");
    let client = Client::init(TEST_USER, TEST_PASS, &base_url).expect("client init");

    // GET and PUT both carry the Basic-auth header; check the read and write paths.
    let get_res = client.get(&format!("{base_url}file.txt")).unwrap();
    assert_eq!(get_res.status().as_u16(), 302, "3xx must surface as-is");
    let put_res = client
        .put(b"payload".to_vec(), &format!("{base_url}upload.txt"))
        .unwrap();
    assert_eq!(put_res.status().as_u16(), 302, "3xx must surface as-is");

    std::thread::sleep(Duration::from_millis(200));
    server_stop.store(true, Ordering::SeqCst);
    attacker_stop.store(true, Ordering::SeqCst);
    let _ = server_thread.join();
    let _ = attacker_thread.join();

    let server_reqs = server_requests
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .clone();
    let attacker_reqs = attacker_requests
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .clone();

    // The configured server saw both authenticated requests...
    assert_eq!(
        server_reqs.len(),
        2,
        "expected GET and PUT, got: {server_reqs:?}"
    );
    assert!(
        !server_reqs
            .iter()
            .any(|r| r.contains("(no authorization header)")),
        "configured server never saw the credentials: {server_reqs:?}"
    );
    // ...and the redirect target saw nothing: no followed redirect, no leak.
    assert!(
        attacker_reqs.is_empty(),
        "CREDENTIAL LEAK: redirect target received {} request(s): {attacker_reqs:?}",
        attacker_reqs.len()
    );
}
