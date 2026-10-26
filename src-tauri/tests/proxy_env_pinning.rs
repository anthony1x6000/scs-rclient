//! Ambient proxy configuration must not reroute the pinned WebDAV transport.
//!
//! dav_client pins the validated addresses with resolve_to_addrs and documents
//! "the address the guard checked is the address the sockets dial". reqwest
//! enables system-proxy detection by default, so an HTTP_PROXY/ALL_PROXY in
//! the launch environment would make every request dial the proxy instead —
//! an address the IP blocklist never evaluated — and hand the proxy the
//! resolution of the WebDAV host. These tests pin the invariant: with proxy
//! variables set, requests still arrive at the pinned base listener and the
//! proxy receives nothing.

use scs_rclient_lib::dav_client::Client;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

fn start_listener(
    body: &'static str,
) -> (u16, Arc<Mutex<Vec<String>>>, Arc<AtomicBool>, std::thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let hits: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let stop = Arc::new(AtomicBool::new(false));
    let hits_clone = hits.clone();
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
                    let first_line = String::from_utf8_lossy(&buf).into_owned();
                    let first_line = first_line.lines().next().unwrap_or("").to_string();
                    hits_clone.lock().unwrap_or_else(|p| p.into_inner()).push(first_line.clone());
                    let resp = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
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
    (port, hits, stop, handle)
}

fn probe_with_proxy(proxy_var: &str) {
    std::env::set_var("ALLOW_INSECURE_WEBDAV", "1");
    std::env::set_var("WEBDAV_CACHE_DISABLED", "1");

    let (base_port, base_hits, base_stop, base_handle) = start_listener("base-ok");
    let (proxy_port, proxy_hits, proxy_stop, proxy_handle) = start_listener("proxy-must-not-see-this");
    std::env::set_var(proxy_var, format!("http://127.0.0.1:{proxy_port}/"));

    let base_url = format!("http://127.0.0.1:{base_port}/dav/");
    let client = Client::init("", "", &base_url).expect("client init");
    let _ = client.get(&base_url);

    std::thread::sleep(Duration::from_millis(300));
    std::env::remove_var(proxy_var);
    base_stop.store(true, Ordering::SeqCst);
    proxy_stop.store(true, Ordering::SeqCst);
    let _ = base_handle.join();
    let _ = proxy_handle.join();
    std::env::remove_var("ALLOW_INSECURE_WEBDAV");

    let proxy_seen = proxy_hits.lock().unwrap_or_else(|p| p.into_inner()).clone();
    assert!(
        proxy_seen.is_empty(),
        "{proxy_var} rerouted the pinned transport to the proxy: {proxy_seen:?}"
    );
    let base_seen = base_hits.lock().unwrap_or_else(|p| p.into_inner()).clone();
    assert!(
        base_seen.iter().any(|r| r.contains("GET /dav/")),
        "request must reach the pinned base listener directly, saw: {base_seen:?}"
    );
}

#[test]
fn test_http_proxy_cannot_reroute_pinned_transport() {
    probe_with_proxy("HTTP_PROXY");
}

#[test]
fn test_all_proxy_cannot_reroute_pinned_transport() {
    probe_with_proxy("ALL_PROXY");
}
