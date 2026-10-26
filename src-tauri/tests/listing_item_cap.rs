//! A server-controlled listing must not collect items without an aggregate
//! bound. One listing advertising more than MAX_REMOTE_ITEMS files must fail
//! the action loudly instead of materializing unbounded state (and, via the
//! get arm, unbounded local files).

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

#[test]
fn test_listing_item_count_is_bounded() {
    let _allow_insecure = EnvRestore::set("ALLOW_INSECURE_WEBDAV", "1");
    let _cache_disabled = EnvRestore::set("WEBDAV_CACHE_DISABLED", "1");

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let hits: Arc<Mutex<usize>> = Arc::new(Mutex::new(0));
    let hits_clone = hits.clone();
    let handle = std::thread::spawn(move || {
        for stream in listener.incoming() {
            let mut stream = stream.unwrap();
            let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
            let mut buf = Vec::new();
            let mut chunk = [0u8; 8192];
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
            *hits_clone.lock().unwrap() += 1;
            let mut body = String::from("<?xml version=\"1.0\" encoding=\"utf-8\" ?><D:multistatus xmlns:D=\"DAV:\">");
            body.push_str("<D:response><D:href>/course/</D:href><D:propstat><D:prop><D:resourcetype><D:collection/></D:resourcetype></D:prop><D:status>HTTP/1.1 200 OK</D:status></D:propstat></D:response>");
            for i in 0..150_000 {
                body.push_str(&format!(
                    "<D:response><D:href>/course/f{i}.txt</D:href><D:propstat><D:prop><D:resourcetype/><D:getcontentlength>1</D:getcontentlength></D:prop><D:status>HTTP/1.1 200 OK</D:status></D:propstat></D:response>"
                ));
            }
            body.push_str("</D:multistatus>");
            let resp = format!(
                "HTTP/1.1 207 Multi-Status\r\nContent-Type: application/xml; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            let _ = stream.write_all(resp.as_bytes());
        }
    });

    let base_url = format!("http://127.0.0.1:{port}/course/");
    let client = scs_rclient_lib::dav_client::Client::init("", "", &base_url).unwrap();
    let local_dir = std::env::temp_dir().join(format!("scs-itemcap-{}", std::process::id()));
    std::fs::create_dir_all(&local_dir).unwrap();
    let cancel = Arc::new(AtomicBool::new(false));
    let mut logs = Vec::new();
    let result = execute_webdav_action_with_exclusions(
        &client,
        "ls",
        &base_url,
        &local_dir,
        &cancel,
        None,
        None,
        &[],
        |m: &str| logs.push(m.to_string()),
    );
    let _ = std::fs::remove_dir_all(&local_dir);

    assert!(
        result.is_err(),
        "a 150k-item listing must be refused, got Ok (first log lines: {:?})",
        logs.iter().take(4).collect::<Vec<_>>()
    );
    assert!(
        result.unwrap_err().to_ascii_lowercase().contains("item limit"),
        "error must name the item limit"
    );
    drop(handle);
}
