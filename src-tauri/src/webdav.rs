use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct WebdavItem {
    pub href: String,
    pub is_dir: bool,
    pub size: u64,
}

/// Parses the WebDAV PROPFIND XML response into WebdavItem records.
pub fn parse_propfind_xml(xml: &str) -> Vec<WebdavItem> {
    let mut items = Vec::new();
    let mut reader = quick_xml::Reader::from_str(xml);
    reader.config_mut().trim_text(true);

    let mut in_response = false;
    let mut in_href = false;
    let mut in_resourcetype = false;
    let mut in_getcontentlength = false;

    let mut current_href = String::new();
    let mut current_is_dir = false;
    let mut current_size: u64 = 0;

    let mut buf = Vec::new();

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(quick_xml::events::Event::Start(ref e)) => {
                let local = e.local_name();
                match local.as_ref() {
                    b"response" => {
                        in_response = true;
                        current_href.clear();
                        current_is_dir = false;
                        current_size = 0;
                    }
                    b"href" if in_response => in_href = true,
                    b"resourcetype" if in_response => in_resourcetype = true,
                    b"collection" if in_resourcetype => current_is_dir = true,
                    b"getcontentlength" if in_response => in_getcontentlength = true,
                    _ => {}
                }
            }
            Ok(quick_xml::events::Event::Empty(ref e)) => {
                let local = e.local_name();
                if local.as_ref() == b"collection" && in_resourcetype {
                    current_is_dir = true;
                }
            }
            Ok(quick_xml::events::Event::Text(ref e)) => {
                if in_href {
                    if let Ok(text) = e.unescape() {
                        current_href = text.to_string();
                    }
                } else if in_getcontentlength {
                    if let Ok(text) = e.unescape() {
                        current_size = text.trim().parse::<u64>().unwrap_or(0);
                    }
                }
            }
            Ok(quick_xml::events::Event::End(ref e)) => {
                let local = e.local_name();
                match local.as_ref() {
                    b"response" => {
                        in_response = false;
                        if !current_href.is_empty() {
                            items.push(WebdavItem {
                                href: current_href.clone(),
                                is_dir: current_is_dir,
                                size: current_size,
                            });
                        }
                    }
                    b"href" => in_href = false,
                    b"resourcetype" => in_resourcetype = false,
                    b"getcontentlength" => in_getcontentlength = false,
                    _ => {}
                }
            }
            Ok(quick_xml::events::Event::Eof) => break,
            Err(_) => break,
            _ => {}
        }
        buf.clear();
    }

    items
}

/// Simple percent decoder for URI path segments.
fn decode_percent(s: &str) -> String {
    let mut result = Vec::new();
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Ok(byte) = u8::from_str_radix(std::str::from_utf8(&bytes[i + 1..i + 3]).unwrap_or(""), 16) {
                result.push(byte);
                i += 3;
                continue;
            }
        }
        result.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&result).to_string()
}

/// Resolves an item's href against the base URL, handling absolute URLs,
/// absolute paths (/...), and relative paths.
pub fn resolve_item_url(base_url: &str, item_href: &str) -> String {
    if let Ok(base) = rustydav::prelude::Url::parse(base_url) {
        if let Ok(joined) = base.join(item_href) {
            let mut s = joined.to_string();
            if item_href.ends_with('/') && !s.ends_with('/') {
                s.push('/');
            }
            return s;
        }
    }
    let clean_base = base_url.trim_end_matches('/');
    let clean_href = item_href.trim_start_matches('/');
    format!("{}/{}", clean_base, clean_href)
}

/// Computes the item path relative to the collection URL.
pub fn relative_item_path(base_url: &str, item_href: &str) -> String {
    let resolved_url_str = resolve_item_url(base_url, item_href);
    let resolved_url = match rustydav::prelude::Url::parse(&resolved_url_str) {
        Ok(u) => u,
        Err(_) => return decode_percent(item_href).trim_matches('/').to_string(),
    };
    let base_url_parsed = match rustydav::prelude::Url::parse(base_url) {
        Ok(u) => u,
        Err(_) => return decode_percent(item_href).trim_matches('/').to_string(),
    };

    let base_path = decode_percent(base_url_parsed.path());
    let item_path = decode_percent(resolved_url.path());

    let trimmed_base = base_path.trim_end_matches('/');
    let trimmed_item = item_path.trim_end_matches('/');

    if trimmed_item == trimmed_base {
        return String::new();
    }

    if let Some(rel) = trimmed_item.strip_prefix(trimmed_base) {
        rel.trim_start_matches('/').to_string()
    } else {
        trimmed_item.rsplit('/').next().unwrap_or("").to_string()
    }
}

/// Builds the canonical remote collection URL with trailing slash.
pub fn build_remote_url(base_url: &str, subdir: &str) -> String {
    let clean_base = base_url.trim().trim_end_matches('/');
    let clean_sub = subdir.trim().trim_matches('/');
    if clean_sub.is_empty() {
        format!("{}/", clean_base)
    } else {
        format!("{}/{}/", clean_base, clean_sub)
    }
}

/// Builds the URL for a specific file under a collection, ensuring proper percent-encoding.
pub fn build_file_url(collection_url: &str, rel_path: &str) -> String {
    if let Ok(mut base) = rustydav::prelude::Url::parse(collection_url) {
        if let Ok(mut segments) = base.path_segments_mut() {
            segments.pop_if_empty();
            for part in rel_path.split('/') {
                let trimmed = part.trim();
                if !trimmed.is_empty() {
                    segments.push(trimmed);
                }
            }
            return base.to_string();
        }
    }
    let clean_col = collection_url.trim_end_matches('/');
    let clean_rel = rel_path.trim_start_matches('/');
    format!("{}/{}", clean_col, clean_rel)
}

/// Ensures all parent collections exist for a relative file path prior to PUT.
pub fn ensure_remote_parent_dirs(
    client: &rustydav::client::Client,
    remote_url: &str,
    rel_path: &str,
) {
    let parts: Vec<&str> = rel_path.split('/').collect();
    if parts.len() <= 1 {
        return;
    }
    let mut current_rel = String::new();
    for part in &parts[..parts.len() - 1] {
        if !current_rel.is_empty() {
            current_rel.push('/');
        }
        current_rel.push_str(part);
        let dir_url = format!("{}/", build_file_url(remote_url, &current_rel));
        // MKCOL creates the folder; 201 Created or 405 Method Not Allowed (already exists) are expected
        let _ = client.mkcol(&dir_url);
    }
}

/// Recursively lists remote WebDAV items under remote_url.
/// First attempts Depth: infinity. If the server rejects Depth: infinity (e.g. 403 Forbidden or 400 Bad Request),
/// it falls back to breadth-first traversal using Depth: 1.
pub fn list_remote_recursive(
    client: &rustydav::client::Client,
    remote_url: &str,
    cancel_flag: &AtomicBool,
) -> Result<Vec<WebdavItem>, String> {
    if cancel_flag.load(Ordering::SeqCst) {
        return Err("Operation canceled by user.".to_string());
    }

    // Try Depth: infinity first
    if let Ok(res) = client.list(remote_url, "infinity") {
        let status = res.status();
        if status.is_success() || status.as_u16() == 207 {
            let body = res.text().unwrap_or_default();
            let items = parse_propfind_xml(&body);
            if !items.is_empty() {
                return Ok(items);
            }
        }
    }

    // Fallback: BFS traversal with Depth: 1
    let mut queue = vec![remote_url.to_string()];
    let mut visited = HashSet::new();
    let mut all_items = Vec::new();

    visited.insert(remote_url.trim_end_matches('/').to_string());

    while let Some(current_url) = queue.pop() {
        if cancel_flag.load(Ordering::SeqCst) {
            return Err("Operation canceled by user.".to_string());
        }

        let res = client
            .list(&current_url, "1")
            .map_err(|e| format!("List request failed for {}: {}", current_url, e))?;

        let status = res.status();
        if !status.is_success() && status.as_u16() != 207 {
            return Err(format!("Server returned HTTP {} for {}", status, current_url));
        }

        let body = res.text().unwrap_or_default();
        let items = parse_propfind_xml(&body);

        for item in items {
            let rel = relative_item_path(&current_url, &item.href);
            if rel.is_empty() {
                // Skips current collection directory itself
                continue;
            }

            if item.is_dir {
                let sub_url = resolve_item_url(&current_url, &item.href);
                let sub_key = sub_url.trim_end_matches('/').to_string();
                if visited.insert(sub_key) {
                    queue.push(sub_url);
                }
                all_items.push(item);
            } else {
                all_items.push(item);
            }
        }
    }

    Ok(all_items)
}

/// Walks a local directory recursively and returns (path, rel_path, size).
pub fn collect_local_files(dir: &Path) -> Vec<(PathBuf, String, u64)> {
    let mut files = Vec::new();
    if !dir.exists() || !dir.is_dir() {
        return files;
    }
    let mut stack = vec![dir.to_path_buf()];
    while let Some(current_dir) = stack.pop() {
        if let Ok(entries) = std::fs::read_dir(&current_dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    stack.push(path);
                } else if path.is_file() {
                    if let Ok(rel) = path.strip_prefix(dir) {
                        let rel_str = rel.to_string_lossy().replace('\\', "/");
                        let size = entry.metadata().map(|m| m.len()).unwrap_or(0);
                        files.push((path, rel_str, size));
                    }
                }
            }
        }
    }
    files.sort_by(|a, b| a.1.cmp(&b.1));
    files
}

/// Computes SHA256 checksum formatted as hex string.
pub fn compute_sha256(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    format!("{:x}", hasher.finalize())
}

/// Verifies authentication by sending PROPFIND depth 0 to the URL.
pub fn verify_webdav_auth(url: &str, username: &str, password: &str) -> Result<(), String> {
    let client = rustydav::client::Client::init(username, password);
    match client.list(url, "0") {
        Ok(res) => {
            let status = res.status();
            if status.is_success() || status.as_u16() == 207 {
                Ok(())
            } else if status.as_u16() == 401 || status.as_u16() == 403 {
                Err("Authentication failed: invalid username or password.".to_string())
            } else {
                Err(format!(
                    "WebDAV server returned status {}: {}",
                    status.as_u16(),
                    status.canonical_reason().unwrap_or("Unknown")
                ))
            }
        }
        Err(e) => Err(format!("Failed to connect to WebDAV server: {}", e)),
    }
}

/// Executes a native WebDAV action and streams logs through the callback.
pub fn execute_webdav_action<F>(
    client: &rustydav::client::Client,
    action: &str,
    remote_url: &str,
    local_dir: &Path,
    cancel_flag: &AtomicBool,
    mut log: F,
) -> Result<(), String>
where
    F: FnMut(&str),
{
    match action {
        "ls" => {
            log(&format!("Listing remote files in {}...\n", remote_url));
            let items = list_remote_recursive(client, remote_url, cancel_flag)?;
            let mut count = 0;
            let mut total_size = 0;
            for item in &items {
                if item.is_dir {
                    continue;
                }
                let rel = relative_item_path(remote_url, &item.href);
                if rel.is_empty() {
                    continue;
                }
                log(&format!("{:>9} {}\n", item.size, rel));
                count += 1;
                total_size += item.size;
            }
            log(&format!(
                "\nTotal objects: {}, Total size: {} bytes\n",
                count, total_size
            ));
            Ok(())
        }
        "lsd" => {
            log(&format!("Listing remote directories in {}...\n", remote_url));
            let res = client
                .list(remote_url, "1")
                .map_err(|e| format!("List request failed: {}", e))?;
            if !res.status().is_success() && res.status().as_u16() != 207 {
                return Err(format!("Server returned HTTP {}", res.status()));
            }
            let body = res.text().unwrap_or_default();
            let items = parse_propfind_xml(&body);
            let mut count = 0;
            for item in &items {
                if !item.is_dir {
                    continue;
                }
                let rel = relative_item_path(remote_url, &item.href);
                if rel.is_empty() {
                    continue;
                }
                log(&format!("{:>9} {}/\n", -1, rel));
                count += 1;
            }
            log(&format!("\nTotal directories: {}\n", count));
            Ok(())
        }
        "put" | "put-dry" | "put-checksum" => {
            let is_dry = action == "put-dry";
            let with_checksum = action == "put-checksum";
            log(&format!("Reading local files in {}...\n", local_dir.display()));
            let local_files = collect_local_files(local_dir);
            if local_files.is_empty() {
                log("No local files found to copy.\n");
                return Ok(());
            }
            log(&format!(
                "Found {} local file(s). Starting upload...\n",
                local_files.len()
            ));
            for (path, rel_str, size) in local_files {
                if cancel_flag.load(Ordering::SeqCst) {
                    log("Operation canceled by user.\n");
                    return Ok(());
                }
                let file_url = build_file_url(remote_url, &rel_str);
                if is_dry {
                    log(&format!(
                        "NOTICE: {}: Skipped copy (dry run, {} bytes)\n",
                        rel_str, size
                    ));
                    continue;
                }
                ensure_remote_parent_dirs(client, remote_url, &rel_str);
                let bytes = std::fs::read(&path)
                    .map_err(|e| format!("Failed to read {}: {}", path.display(), e))?;
                let checksum_str = if with_checksum {
                    format!(" (sha256: {})", compute_sha256(&bytes))
                } else {
                    String::new()
                };
                let res = client
                    .put(bytes, &file_url)
                    .map_err(|e| format!("Upload failed for {}: {}", rel_str, e))?;
                if res.status().is_success() {
                    log(&format!(
                        "Copied: {} ({} bytes){}\n",
                        rel_str, size, checksum_str
                    ));
                } else {
                    log(&format!(
                        "ERROR: Failed to copy {}: HTTP {}\n",
                        rel_str,
                        res.status()
                    ));
                }
            }
            log("\nPut operation finished.\n");
            Ok(())
        }
        "get" | "get-dry" | "get-checksum" => {
            let is_dry = action == "get-dry";
            let with_checksum = action == "get-checksum";
            log(&format!("Listing remote files in {}...\n", remote_url));
            let items = list_remote_recursive(client, remote_url, cancel_flag)?;
            let mut count = 0;
            let mut total_bytes = 0;
            for item in &items {
                if item.is_dir {
                    continue;
                }
                let rel = relative_item_path(remote_url, &item.href);
                if rel.is_empty() {
                    continue;
                }
                if cancel_flag.load(Ordering::SeqCst) {
                    log("Operation canceled by user.\n");
                    return Ok(());
                }
                if is_dry {
                    log(&format!(
                        "NOTICE: {}: Skipped copy (dry run, {} bytes)\n",
                        rel, item.size
                    ));
                    count += 1;
                    continue;
                }
                let download_url = resolve_item_url(remote_url, &item.href);
                let get_res = client
                    .get(&download_url)
                    .map_err(|e| format!("Download failed for {}: {}", rel, e))?;
                if !get_res.status().is_success() {
                    log(&format!(
                        "ERROR: Failed to download {}: HTTP {}\n",
                        rel,
                        get_res.status()
                    ));
                    continue;
                }
                let bytes = get_res
                    .bytes()
                    .map_err(|e| format!("Failed to read response bytes: {}", e))?;
                let target_file = local_dir.join(&rel);
                if let Some(parent) = target_file.parent() {
                    std::fs::create_dir_all(parent).map_err(|e| {
                        format!("Failed to create directory {}: {}", parent.display(), e)
                    })?;
                }
                std::fs::write(&target_file, &bytes)
                    .map_err(|e| format!("Failed to write {}: {}", target_file.display(), e))?;
                let checksum_str = if with_checksum {
                    format!(" (sha256: {})", compute_sha256(&bytes))
                } else {
                    String::new()
                };
                count += 1;
                total_bytes += bytes.len();
                log(&format!(
                    "Downloaded: {} ({} bytes){}\n",
                    rel,
                    bytes.len(),
                    checksum_str
                ));
            }
            log(&format!(
                "\nGet operation finished: {} file(s) downloaded ({} bytes).\n",
                count, total_bytes
            ));
            Ok(())
        }
        "check" => {
            log(&format!("Comparing local files with remote in {}...\n", remote_url));
            let remote_items = list_remote_recursive(client, remote_url, cancel_flag)?;
            let mut remote_map: HashMap<String, u64> = HashMap::new();
            for item in remote_items {
                if item.is_dir {
                    continue;
                }
                let rel = relative_item_path(remote_url, &item.href);
                if !rel.is_empty() {
                    remote_map.insert(rel, item.size);
                }
            }
            let local_files = collect_local_files(local_dir);
            let mut matching = 0;
            let mut differences = 0;
            for (_, rel, local_size) in &local_files {
                match remote_map.remove(rel) {
                    Some(rem_size) => {
                        if *local_size == rem_size {
                            matching += 1;
                        } else {
                            differences += 1;
                            log(&format!(
                                "Difference in size: {} (local: {} bytes, remote: {} bytes)\n",
                                rel, local_size, rem_size
                            ));
                        }
                    }
                    None => {
                        differences += 1;
                        log(&format!("Missing in remote: {}\n", rel));
                    }
                }
            }
            for (rem_rel, _) in remote_map {
                differences += 1;
                log(&format!("Missing in local: {}\n", rem_rel));
            }
            log(&format!(
                "\nCheck summary: {} matching, {} differences.\n",
                matching, differences
            ));
            Ok(())
        }
        "sync" => {
            log(&format!(
                "Starting sync from {} to {}...\n",
                local_dir.display(),
                remote_url
            ));
            let local_files = collect_local_files(local_dir);
            let mut local_set: HashSet<String> = HashSet::new();
            let mut uploaded = 0;
            for (path, rel, size) in local_files {
                if cancel_flag.load(Ordering::SeqCst) {
                    log("Operation canceled by user.\n");
                    return Ok(());
                }
                local_set.insert(rel.clone());
                let file_url = build_file_url(remote_url, &rel);
                ensure_remote_parent_dirs(client, remote_url, &rel);
                let bytes = std::fs::read(&path)
                    .map_err(|e| format!("Read error {}: {}", path.display(), e))?;
                let res = client
                    .put(bytes, &file_url)
                    .map_err(|e| format!("Upload error: {}", e))?;
                if res.status().is_success() {
                    log(&format!("Synced: {} ({} bytes)\n", rel, size));
                    uploaded += 1;
                }
            }
            let remote_items = list_remote_recursive(client, remote_url, cancel_flag)?;
            let mut deleted = 0;
            for item in remote_items {
                if item.is_dir {
                    continue;
                }
                let rel = relative_item_path(remote_url, &item.href);
                if !rel.is_empty() && !local_set.contains(&rel) {
                    if cancel_flag.load(Ordering::SeqCst) {
                        log("Operation canceled by user.\n");
                        return Ok(());
                    }
                    let file_url = resolve_item_url(remote_url, &item.href);
                    let del_res = client.delete(&file_url);
                    if del_res.is_ok() {
                        log(&format!("Deleted remote file not in local: {}\n", rel));
                        deleted += 1;
                    }
                }
            }
            log(&format!(
                "\nSync complete: {} synced, {} remote files removed.\n",
                uploaded, deleted
            ));
            Ok(())
        }
        other => Err(format!("Unsupported WebDAV action: {}", other)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_propfind() {
        let sample = r#"<?xml version="1.0" encoding="utf-8"?>
        <D:multistatus xmlns:D="DAV:">
            <D:response>
                <D:href>/files/myuser/docs/</D:href>
                <D:propstat>
                    <D:prop>
                        <D:resourcetype><D:collection/></D:resourcetype>
                    </D:prop>
                    <D:status>HTTP/1.1 200 OK</D:status>
                </D:propstat>
            </D:response>
            <D:response>
                <D:href>/files/myuser/docs/notes.txt</D:href>
                <D:propstat>
                    <D:prop>
                        <D:getcontentlength>1024</D:getcontentlength>
                        <D:resourcetype/>
                    </D:prop>
                    <D:status>HTTP/1.1 200 OK</D:status>
                </D:propstat>
            </D:response>
        </D:multistatus>"#;

        let items = parse_propfind_xml(sample);
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].href, "/files/myuser/docs/");
        assert!(items[0].is_dir);
        assert_eq!(items[1].href, "/files/myuser/docs/notes.txt");
        assert!(!items[1].is_dir);
        assert_eq!(items[1].size, 1024);
    }

    #[test]
    fn test_resolve_item_url() {
        let base = "https://example.com/remote.php/webdav/folder/";
        assert_eq!(
            resolve_item_url(base, "/remote.php/webdav/folder/file.txt"),
            "https://example.com/remote.php/webdav/folder/file.txt"
        );
        assert_eq!(
            resolve_item_url(base, "sub/file.txt"),
            "https://example.com/remote.php/webdav/folder/sub/file.txt"
        );
        assert_eq!(
            resolve_item_url(base, "https://example.com/remote.php/webdav/folder/file.txt"),
            "https://example.com/remote.php/webdav/folder/file.txt"
        );
    }

    #[test]
    fn test_relative_item_path() {
        let base = "https://example.com/remote.php/webdav/folder/";
        assert_eq!(relative_item_path(base, "/remote.php/webdav/folder/"), "");
        assert_eq!(relative_item_path(base, "/remote.php/webdav/folder/file.txt"), "file.txt");
        assert_eq!(relative_item_path(base, "/remote.php/webdav/folder/sub/data.csv"), "sub/data.csv");
        assert_eq!(relative_item_path(base, "https://example.com/remote.php/webdav/folder/sub/data.csv"), "sub/data.csv");
        assert_eq!(relative_item_path(base, "/remote.php/webdav/folder/my%20folder/file%201.txt"), "my folder/file 1.txt");

        let root_base = "http://localhost:3923/";
        assert_eq!(relative_item_path(root_base, "/"), "");
        assert_eq!(relative_item_path(root_base, "/file.txt"), "file.txt");
        assert_eq!(relative_item_path(root_base, "http://localhost:3923/"), "");
        assert_eq!(relative_item_path(root_base, "http://localhost:3923/file.txt"), "file.txt");
    }

    #[test]
    fn test_build_file_url_spaces() {
        let base = "http://localhost:3923/docs/";
        let url = build_file_url(base, "my folder/my file.txt");
        assert_eq!(url, "http://localhost:3923/docs/my%20folder/my%20file.txt");
        assert!(rustydav::prelude::Url::parse(&url).is_ok());
    }
}
