use scs_rclient_lib::dav_client::Client;
use scs_rclient_lib::webdav::{
    ExtensionFilter, ExtensionFilterMode, build_file_url, collect_local_files,
    collect_local_files_excluding, compute_sha256, execute_webdav_action,
    execute_webdav_action_with_exclusions, list_remote_recursive,
    list_remote_recursive_with_exclusions_and_log, list_remote_recursive_with_options_and_log,
    normalize_exclude_folders, parse_propfind_xml, parse_webdav_date, relative_item_path,
    resolve_item_url, should_download_file, should_upload_file, verify_webdav_auth,
};
use std::collections::HashSet;
use std::fs;
use std::sync::atomic::AtomicBool;

/// Returns the live WebDAV base URL, or `None` when the e2e server is not configured.
fn live_base_url() -> Option<String> {
    match std::env::var("TEST_WEBDAV_URL") {
        Ok(url) if !url.trim().is_empty() => Some(url),
        _ => None,
    }
}

fn live_client() -> Client {
    let username = std::env::var("TEST_WEBDAV_USER").unwrap_or_else(|_| "testuser".to_string());
    let password = std::env::var("TEST_WEBDAV_PASS").unwrap_or_else(|_| "testpass".to_string());
    let base_url = live_base_url().expect("TEST_WEBDAV_URL configured");
    Client::init(&username, &password, &base_url).expect("client init")
}

fn write_local_file(root: &std::path::Path, rel: &str, contents: &[u8]) {
    let path = root.join(rel);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, contents).unwrap();
}

fn local_file_paths(root: &std::path::Path) -> Vec<String> {
    let mut paths: Vec<String> = collect_local_files(root)
        .into_iter()
        .map(|(_, rel, _)| rel)
        .collect();
    paths.sort();
    paths
}

/// Lists remote files relative to `url`, bypassing the listing cache so assertions see
/// the server's current state rather than a listing cached by an earlier action.
fn remote_file_paths(
    client: &Client,
    url: &str,
    cancel_flag: &AtomicBool,
) -> HashSet<String> {
    list_remote_recursive_with_options_and_log(
        client,
        url,
        cancel_flag,
        None,
        None,
        false,
        |_| {},
    )
    .expect("remote listing failed")
    .iter()
    .filter(|item| !item.is_dir)
    .map(|item| relative_item_path(url, &item.href))
    .collect()
}

fn read_remote_text(client: &Client, url: &str) -> String {
    use std::io::Read;
    let mut response = client.get(url).expect("remote GET failed");
    assert!(
        response.status().is_success(),
        "remote GET {} returned HTTP {}",
        url,
        response.status()
    );
    let mut body = String::new();
    response
        .read_to_string(&mut body)
        .expect("failed to read remote body");
    body
}

/// Deletes every child of `url` (files first, deepest directories last) and then `url`.
fn remove_remote_tree(client: &Client, url: &str, cancel_flag: &AtomicBool) {
    if let Ok(items) = list_remote_recursive_with_options_and_log(
        client,
        url,
        cancel_flag,
        None,
        None,
        false,
        |_| {},
    ) {
        for item in items.iter().filter(|item| !item.is_dir) {
            let _ = client.delete(&resolve_item_url(url, &item.href));
        }
        let mut directories: Vec<&String> = items
            .iter()
            .filter(|item| item.is_dir)
            .map(|item| &item.href)
            .collect();
        directories.sort_by(|a, b| b.cmp(a));
        for href in directories {
            let _ = client.delete(&resolve_item_url(url, href));
        }
    }
    let _ = client.delete(url);
}

/// Extracts relative directory paths from `[n] Scanning directory: <path>...` log lines.
fn scanned_directories(log: &str) -> Vec<String> {
    log.lines()
        .filter_map(|line| line.split_once("Scanning directory: ").map(|(_, rest)| rest))
        .map(|rest| {
            rest.trim_end_matches("...")
                .trim_end_matches('/')
                .trim()
                .to_string()
        })
        .filter(|dir| !dir.is_empty() && dir != "/ (root)")
        .collect()
}

fn assert_scanned_directories(log: &str, expected: &[&str]) {
    let scanned = scanned_directories(log);
    for dir in expected {
        assert!(
            scanned.iter().any(|scanned_dir| scanned_dir == dir),
            "included folder {} must be scanned; scanned: {:?}\nlog:\n{}",
            dir,
            scanned,
            log
        );
    }
}

fn assert_not_scanned_directories(log: &str, excluded: &[&str]) {
    let scanned = scanned_directories(log);
    for dir in excluded {
        assert!(
            !scanned.iter().any(|scanned_dir| scanned_dir == dir),
            "excluded folder {} must not be scanned; scanned: {:?}\nlog:\n{}",
            dir,
            scanned,
            log
        );
    }
}

#[test]
fn test_incremental_upload_decision_matrix() {
    // 1. Different sizes -> must always upload
    assert!(should_upload_file(100, Some(1000), 200, Some(1000)));
    assert!(should_upload_file(100, None, 200, None));

    // 2. Same size, local file newer than remote + 1s -> must upload
    assert!(should_upload_file(100, Some(1005), 100, Some(1000)));

    // 3. Same size, local file older or within 1s margin -> skip (already up-to-date)
    assert!(!should_upload_file(100, Some(1000), 100, Some(1000)));
    assert!(!should_upload_file(100, Some(1001), 100, Some(1000)));
    assert!(!should_upload_file(100, Some(990), 100, Some(1000)));

    // 4. Date parsing integration
    let remote_date = parse_webdav_date("Mon, 28 Sep 2026 13:45:09 GMT").unwrap();
    let local_newer = remote_date + 60;
    let local_older = remote_date - 60;
    assert!(should_upload_file(500, Some(local_newer), 500, Some(remote_date)));
    assert!(!should_upload_file(500, Some(local_older), 500, Some(remote_date)));
    assert!(!should_upload_file(500, Some(remote_date), 500, Some(remote_date)));
}

#[test]
fn test_incremental_download_decision_matrix() {
    // 1. Different sizes -> must always download
    assert!(should_download_file(200, Some(1000), 100, Some(1000)));
    assert!(should_download_file(200, None, 100, None));

    // 2. Same size, remote file newer than local + 1s -> must download
    assert!(should_download_file(100, Some(1005), 100, Some(1000)));

    // 3. Same size, remote file older or within 1s margin -> skip (already up-to-date)
    assert!(!should_download_file(100, Some(1000), 100, Some(1000)));
    assert!(!should_download_file(100, Some(1001), 100, Some(1000)));
    assert!(!should_download_file(100, Some(990), 100, Some(1000)));

    // 4. Date parsing integration
    let remote_date = parse_webdav_date("Mon, 28 Sep 2026 13:45:09 GMT").unwrap();
    let local_newer = remote_date + 60;
    let local_older = remote_date - 60;
    assert!(should_download_file(500, Some(remote_date), 500, Some(local_older)));
    assert!(!should_download_file(500, Some(remote_date), 500, Some(local_newer)));
    assert!(!should_download_file(500, Some(remote_date), 500, Some(remote_date)));
}

#[test]
fn test_xml_parsing_robustness() {
    let xml = r#"<?xml version="1.0" encoding="utf-8"?>
    <D:multistatus xmlns:D="DAV:">
        <D:response>
            <D:href>/webdav/</D:href>
            <D:propstat>
                <D:prop>
                    <D:resourcetype><D:collection/></D:resourcetype>
                </D:prop>
                <D:status>HTTP/1.1 200 OK</D:status>
            </D:propstat>
        </D:response>
        <D:response>
            <D:href>/webdav/document.pdf</D:href>
            <D:propstat>
                <D:prop>
                    <D:getcontentlength>204800</D:getcontentlength>
                    <D:resourcetype/>
                </D:prop>
                <D:status>HTTP/1.1 200 OK</D:status>
            </D:propstat>
        </D:response>
        <D:response>
            <D:href>/webdav/folder%20name/</D:href>
            <D:propstat>
                <D:prop>
                    <D:resourcetype><D:collection/></D:resourcetype>
                </D:prop>
                <D:status>HTTP/1.1 200 OK</D:status>
            </D:propstat>
        </D:response>
    </D:multistatus>"#;

    let items = parse_propfind_xml(xml);
    assert_eq!(items.len(), 3);
    assert!(items[0].is_dir);
    assert_eq!(items[0].href, "/webdav/");
    assert!(!items[1].is_dir);
    assert_eq!(items[1].href, "/webdav/document.pdf");
    assert_eq!(items[1].size, 204800);
    assert!(items[2].is_dir);
    assert_eq!(items[2].href, "/webdav/folder%20name/");
}

#[test]
fn test_relative_item_path_edge_cases() {
    let base = "http://127.0.0.1:3923/docs/";

    // 1. Directory itself
    assert_eq!(relative_item_path(base, "/docs/"), "");
    assert_eq!(relative_item_path(base, "http://127.0.0.1:3923/docs/"), "");

    // 2. Direct child file
    assert_eq!(relative_item_path(base, "/docs/file.txt"), "file.txt");
    assert_eq!(
        relative_item_path(base, "http://127.0.0.1:3923/docs/file.txt"),
        "file.txt"
    );

    // 3. Nested child file
    assert_eq!(
        relative_item_path(base, "/docs/sub/nested/file.txt"),
        "sub/nested/file.txt"
    );
    assert_eq!(
        relative_item_path(base, "http://127.0.0.1:3923/docs/sub/nested/file.txt"),
        "sub/nested/file.txt"
    );

    // 4. File with spaces and percent-encoding
    assert_eq!(
        relative_item_path(base, "/docs/my%20folder/my%20file.txt"),
        "my folder/my file.txt"
    );
    assert_eq!(
        relative_item_path(base, "http://127.0.0.1:3923/docs/my%20folder/my%20file.txt"),
        "my folder/my file.txt"
    );

    // 5. Root collection base URL
    let root = "http://127.0.0.1:3923/";
    assert_eq!(relative_item_path(root, "/"), "");
    assert_eq!(relative_item_path(root, "http://127.0.0.1:3923/"), "");
    assert_eq!(relative_item_path(root, "/test.log"), "test.log");
    assert_eq!(
        relative_item_path(root, "http://127.0.0.1:3923/test.log"),
        "test.log"
    );
}

#[test]
fn test_url_encoding_for_spaces() {
    let base = "http://127.0.0.1:3923/docs/";
    let built = build_file_url(base, "folder name/file with spaces.txt");
    assert_eq!(
        built,
        "http://127.0.0.1:3923/docs/folder%20name/file%20with%20spaces.txt"
    );
    assert!(reqwest::Url::parse(&built).is_ok());
}

#[test]
fn test_iis_depth_infinity_degradation_detected() {
    // When IIS restricts Depth: infinity, it returns only the root collection.
    // parse_propfind_xml parses it, but child items filter must detect 0 children
    // so list_remote_recursive does not falsely claim success and instead falls back to Depth: 1.
    let base = "https://courselinkdav.desire2learn.com/content/enforced/1052175-dev_asteve18/";
    let xml = r#"<?xml version="1.0" encoding="utf-8"?>
    <D:multistatus xmlns:D="DAV:">
        <D:Response>
            <D:Href>/content/enforced/1052175-dev_asteve18/</D:Href>
            <D:PropStat>
                <D:Prop>
                    <D:ResourceType><D:Collection/></D:ResourceType>
                </D:Prop>
                <D:Status>HTTP/1.1 200 OK</D:Status>
            </D:PropStat>
        </D:Response>
    </D:multistatus>"#;

    let items = parse_propfind_xml(xml);
    assert_eq!(items.len(), 1);
    assert!(items[0].is_dir);

    let rel = relative_item_path(base, &items[0].href);
    assert_eq!(rel, "", "Root collection must have empty relative path");

    let child_count = items
        .iter()
        .filter(|i| !relative_item_path(base, &i.href).is_empty())
        .count();
    assert_eq!(child_count, 0, "No child items found in Depth: 0 response");
}

#[test]
fn test_live_copyparty_e2e_full_roundtrip() {
    let base_url = match std::env::var("TEST_WEBDAV_URL") {
        Ok(v) if !v.trim().is_empty() => v,
        _ => {
            eprintln!("SKIPPED: TEST_WEBDAV_URL not set; skipping live Copyparty roundtrip test.");
            return;
        }
    };
    let username = std::env::var("TEST_WEBDAV_USER").unwrap_or_else(|_| "testuser".to_string());
    let password = std::env::var("TEST_WEBDAV_PASS").unwrap_or_else(|_| "testpass".to_string());

    let cancel_flag = AtomicBool::new(false);
    let mut log_output = String::new();
    let client = Client::init(&username, &password, &base_url)
        .expect("client init");

    // 1. Verify Authentication
    assert!(
        verify_webdav_auth(&base_url, &username, &password).is_ok(),
        "verify_webdav_auth should succeed with valid credentials"
    );
    assert!(
        verify_webdav_auth(&base_url, &username, "wrongpassword").is_err(),
        "verify_webdav_auth should fail with invalid credentials"
    );

    // 2. Prepare local test directories and files mimicking D2L course layout
    let tmp_test_dir = std::env::temp_dir().join(format!(
        "scs_test_{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let local_sync_dir = tmp_test_dir.join("local_files");
    fs::create_dir_all(&local_sync_dir).unwrap();

    // File 1: .gitignore (hidden file in root)
    let gitignore_path = local_sync_dir.join(".gitignore");
    let gitignore_content = "target/\n*.log\nnode_modules/\n";
    fs::write(&gitignore_path, gitignore_content).unwrap();
    let gitignore_sha256 = compute_sha256(gitignore_content.as_bytes());

    // File 2: index.html (root HTML file)
    let index_path = local_sync_dir.join("index.html");
    let index_content = "<html><head><title>Course Homepage</title></head><body><h1>Welcome</h1></body></html>";
    fs::write(&index_path, index_content).unwrap();
    let index_sha256 = compute_sha256(index_content.as_bytes());

    // File 3: after quiz and survey.html (root file with spaces in filename)
    let spaces_path = local_sync_dir.join("after quiz and survey.html");
    let spaces_content = "<p>Thank you for completing the survey and quiz!</p>";
    fs::write(&spaces_path, spaces_content).unwrap();
    let spaces_sha256 = compute_sha256(spaces_content.as_bytes());

    // File 4: Unit03_MATH1060DE_S26.docx (root binary DOCX document)
    let docx_path = local_sync_dir.join("Unit03_MATH1060DE_S26.docx");
    let docx_bytes = vec![0x50, 0x4b, 0x03, 0x04, 0x14, 0x00, 0x06, 0x00, 0xde, 0xad, 0xbe, 0xef, 0x42];
    fs::write(&docx_path, &docx_bytes).unwrap();
    let docx_sha256 = compute_sha256(&docx_bytes);

    // File 5: Assets/icons/banner.png (Depth 2 nested binary asset)
    let assets_dir = local_sync_dir.join("Assets").join("icons");
    fs::create_dir_all(&assets_dir).unwrap();
    let banner_path = assets_dir.join("banner.png");
    let banner_bytes = vec![0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d];
    fs::write(&banner_path, &banner_bytes).unwrap();
    let banner_sha256 = compute_sha256(&banner_bytes);

    // File 6: css/themes/dark/style.css (Depth 3 deeply nested directory)
    let css_dir = local_sync_dir.join("css").join("themes").join("dark");
    fs::create_dir_all(&css_dir).unwrap();
    let css_path = css_dir.join("style.css");
    let css_content = "body { background-color: #121212; color: #ffffff; }";
    fs::write(&css_path, css_content).unwrap();
    let css_sha256 = compute_sha256(css_content.as_bytes());

    // File 7: .gemini/settings.json (Hidden directory file)
    let gemini_dir = local_sync_dir.join(".gemini");
    fs::create_dir_all(&gemini_dir).unwrap();
    let gemini_path = gemini_dir.join("settings.json");
    let gemini_content = r#"{"model": "gemini-pro", "version": 2}"#;
    fs::write(&gemini_path, gemini_content).unwrap();
    let gemini_sha256 = compute_sha256(gemini_content.as_bytes());

    // 3. Execute PUT (Upload local files to remote)
    let put_res = execute_webdav_action(
        &client,
        "put",
        &base_url,
        &local_sync_dir,
        &cancel_flag,
        |msg| log_output.push_str(msg),
    );
    assert!(put_res.is_ok(), "PUT action failed: {:?}", put_res);
    assert!(
        log_output.contains("Copied: .gitignore"),
        "PUT output should log .gitignore: {}",
        log_output
    );
    assert!(
        log_output.contains("Copied: index.html"),
        "PUT output should log index.html: {}",
        log_output
    );
    assert!(
        log_output.contains("Copied: after quiz and survey.html"),
        "PUT output should log spaces file: {}",
        log_output
    );
    assert!(
        log_output.contains("Copied: Unit03_MATH1060DE_S26.docx"),
        "PUT output should log docx file: {}",
        log_output
    );
    assert!(
        log_output.contains("Copied: Assets/icons/banner.png"),
        "PUT output should log nested banner: {}",
        log_output
    );
    assert!(
        log_output.contains("Copied: css/themes/dark/style.css"),
        "PUT output should log deep css: {}",
        log_output
    );

    // 3b. INCREMENTAL PUT TEST: Second PUT without modifying files -> must skip all up-to-date files
    log_output.clear();
    let put2_res = execute_webdav_action(
        &client,
        "put",
        &base_url,
        &local_sync_dir,
        &cancel_flag,
        |msg| log_output.push_str(msg),
    );
    assert!(put2_res.is_ok(), "Second PUT failed: {:?}", put2_res);
    assert!(
        log_output.contains("0 file(s) to upload") || log_output.contains("0 file(s) copied"),
        "Second PUT must skip all files and upload 0: {}",
        log_output
    );
    assert!(
        log_output.contains("7 file(s) up to date") || log_output.contains("all files up to date"),
        "Second PUT should report all 7 files up to date: {}",
        log_output
    );

    // 3c. Modify index.html content and add new_file.txt -> PUT should only upload those 2 files
    let extra_file_path = local_sync_dir.join("new_file.txt");
    fs::write(&extra_file_path, "Brand new file").unwrap();
    fs::write(
        &index_path,
        "<html><head><title>Updated Course Homepage</title></head><body><h1>Welcome updated</h1></body></html>",
    )
    .unwrap();

    log_output.clear();
    let put3_res = execute_webdav_action(
        &client,
        "put",
        &base_url,
        &local_sync_dir,
        &cancel_flag,
        |msg| log_output.push_str(msg),
    );
    assert!(put3_res.is_ok(), "Incremental PUT failed: {:?}", put3_res);
    assert!(
        log_output.contains("Copied: new_file.txt"),
        "Incremental PUT must copy new_file.txt: {}",
        log_output
    );
    assert!(
        log_output.contains("Copied: index.html"),
        "Incremental PUT must copy modified index.html: {}",
        log_output
    );
    assert!(
        log_output.contains("2 file(s) to upload"),
        "Incremental PUT should report 2 files to upload: {}",
        log_output
    );

    // Clean up extra file and restore original index.html
    fs::remove_file(&extra_file_path).unwrap();
    let del_extra_url = build_file_url(&base_url, "new_file.txt");
    let _ = client.delete(&del_extra_url);
    fs::write(&index_path, index_content).unwrap();
    let _ = execute_webdav_action(&client, "put", &base_url, &local_sync_dir, &cancel_flag, |_| {});

    // 4. Execute LS (Verify listing finds files recursively across depths 1, 2, 3)
    log_output.clear();
    let ls_res = execute_webdav_action(
        &client,
        "ls",
        &base_url,
        &local_sync_dir,
        &cancel_flag,
        |msg| log_output.push_str(msg),
    );
    assert!(ls_res.is_ok(), "LS action failed: {:?}", ls_res);
    assert!(
        log_output.contains(".gitignore"),
        "LS must include .gitignore; actual output:\n{}",
        log_output
    );
    assert!(
        log_output.contains("index.html"),
        "LS must include index.html; actual output:\n{}",
        log_output
    );
    assert!(
        log_output.contains("after quiz and survey.html"),
        "LS must include spaces file; actual output:\n{}",
        log_output
    );
    assert!(
        log_output.contains("Unit03_MATH1060DE_S26.docx"),
        "LS must include docx file; actual output:\n{}",
        log_output
    );
    assert!(
        log_output.contains("Assets/icons/banner.png"),
        "LS must include depth 2 file Assets/icons/banner.png; actual output:\n{}",
        log_output
    );
    assert!(
        log_output.contains("css/themes/dark/style.css"),
        "LS must include depth 3 file css/themes/dark/style.css; actual output:\n{}",
        log_output
    );
    assert!(
        log_output.contains(".gemini/settings.json"),
        "LS must include hidden dir file .gemini/settings.json; actual output:\n{}",
        log_output
    );

    // 5. Execute LSD (Verify directory listing)
    log_output.clear();
    let lsd_res = execute_webdav_action(
        &client,
        "lsd",
        &base_url,
        &local_sync_dir,
        &cancel_flag,
        |msg| log_output.push_str(msg),
    );
    assert!(lsd_res.is_ok(), "LSD action failed: {:?}", lsd_res);

    // 6. Execute CHECK (Should report 0 differences)
    log_output.clear();
    let check_res = execute_webdav_action(
        &client,
        "check",
        &base_url,
        &local_sync_dir,
        &cancel_flag,
        |msg| log_output.push_str(msg),
    );
    assert!(check_res.is_ok(), "CHECK action failed: {:?}", check_res);
    assert!(
        log_output.contains("0 differences"),
        "CHECK should find 0 differences: {}",
        log_output
    );

    // 7. KEY TEST CASE: DELETE LOCAL DIRECTORY & RUN GET (The Missed Part!)
    fs::remove_dir_all(&local_sync_dir).unwrap();
    fs::create_dir_all(&local_sync_dir).unwrap();

    // Verify local directory is completely empty
    let empty_files = collect_local_files(&local_sync_dir);
    assert!(
        empty_files.is_empty(),
        "Local directory must be empty prior to GET test"
    );

    // Run GET
    log_output.clear();
    let get_res = execute_webdav_action(
        &client,
        "get",
        &base_url,
        &local_sync_dir,
        &cancel_flag,
        |msg| log_output.push_str(msg),
    );
    assert!(get_res.is_ok(), "GET action failed: {:?}", get_res);
    assert!(
        log_output.contains("Downloaded: .gitignore"),
        "GET must download .gitignore: {}",
        log_output
    );
    assert!(
        log_output.contains("Downloaded: index.html"),
        "GET must download index.html: {}",
        log_output
    );
    assert!(
        log_output.contains("Downloaded: after quiz and survey.html"),
        "GET must download after quiz and survey.html: {}",
        log_output
    );
    assert!(
        log_output.contains("Downloaded: Unit03_MATH1060DE_S26.docx"),
        "GET must download docx: {}",
        log_output
    );
    assert!(
        log_output.contains("Downloaded: Assets/icons/banner.png"),
        "GET must download depth 2 Assets/icons/banner.png: {}",
        log_output
    );
    assert!(
        log_output.contains("Downloaded: css/themes/dark/style.css"),
        "GET must download depth 3 css/themes/dark/style.css: {}",
        log_output
    );
    assert!(
        log_output.contains("Downloaded: .gemini/settings.json"),
        "GET must download .gemini/settings.json: {}",
        log_output
    );

    // Verify downloaded files and their SHA256 hashes
    let dl_gitignore = fs::read_to_string(&gitignore_path).expect(".gitignore was not downloaded!");
    assert_eq!(dl_gitignore, gitignore_content);
    assert_eq!(compute_sha256(dl_gitignore.as_bytes()), gitignore_sha256);

    let dl_index = fs::read_to_string(&index_path).expect("index.html was not downloaded!");
    assert_eq!(dl_index, index_content);
    assert_eq!(compute_sha256(dl_index.as_bytes()), index_sha256);

    let dl_spaces = fs::read_to_string(&spaces_path).expect("spaces file was not downloaded!");
    assert_eq!(dl_spaces, spaces_content);
    assert_eq!(compute_sha256(dl_spaces.as_bytes()), spaces_sha256);

    let dl_docx = fs::read(&docx_path).expect("docx was not downloaded!");
    assert_eq!(dl_docx, docx_bytes);
    assert_eq!(compute_sha256(&dl_docx), docx_sha256);

    let dl_banner = fs::read(&banner_path).expect("banner was not downloaded!");
    assert_eq!(dl_banner, banner_bytes);
    assert_eq!(compute_sha256(&dl_banner), banner_sha256);

    let dl_css = fs::read_to_string(&css_path).expect("css was not downloaded!");
    assert_eq!(dl_css, css_content);
    assert_eq!(compute_sha256(dl_css.as_bytes()), css_sha256);

    let dl_gemini = fs::read_to_string(&gemini_path).expect("gemini settings was not downloaded!");
    assert_eq!(dl_gemini, gemini_content);
    assert_eq!(compute_sha256(dl_gemini.as_bytes()), gemini_sha256);

    // 8. Execute SYNC: Delete index.html locally and sync -> index.html deleted on remote
    fs::remove_file(&index_path).unwrap();
    log_output.clear();
    let sync_res = execute_webdav_action(
        &client,
        "sync",
        &base_url,
        &local_sync_dir,
        &cancel_flag,
        |msg| log_output.push_str(msg),
    );
    assert!(sync_res.is_ok(), "SYNC action failed: {:?}", sync_res);
    assert!(
        log_output.contains("Deleted remote file not in local: index.html"),
        "SYNC must delete remote index.html: {}",
        log_output
    );

    // Verify index.html is no longer returned by remote listing
    let remote_after_sync = list_remote_recursive(&client, &base_url, &cancel_flag).unwrap();
    let remote_paths: HashSet<String> = remote_after_sync
        .iter()
        .filter(|i| !i.is_dir)
        .map(|i| relative_item_path(&base_url, &i.href))
        .collect();
    assert!(
        !remote_paths.contains("index.html"),
        "index.html should have been deleted from remote"
    );
    assert!(remote_paths.contains(".gitignore"));
    assert!(remote_paths.contains("after quiz and survey.html"));
    assert!(remote_paths.contains("Unit03_MATH1060DE_S26.docx"));
    assert!(remote_paths.contains("Assets/icons/banner.png"));
    assert!(remote_paths.contains("css/themes/dark/style.css"));
    assert!(remote_paths.contains(".gemini/settings.json"));

    // Clean up remote files created in full roundtrip test
    if let Ok(items) = list_remote_recursive(&client, &base_url, &cancel_flag) {
        for item in items {
            let rel = relative_item_path(&base_url, &item.href);
            if !rel.is_empty() {
                let file_url = resolve_item_url(&base_url, &item.href);
                let _ = client.delete(&file_url);
            }
        }
    }

    let _ = fs::remove_dir_all(&tmp_test_dir);
}

#[test]
fn test_live_copyparty_incremental_put_timestamp_differentiation() {
    let base_url = match std::env::var("TEST_WEBDAV_URL") {
        Ok(url) => format!("{}/incremental_test/", url.trim_end_matches('/')),
        Err(_) => {
            eprintln!("TEST_WEBDAV_URL not set, skipping live incremental test");
            return;
        }
    };
    let user = std::env::var("TEST_WEBDAV_USER").unwrap_or_else(|_| "testuser".into());
    let pass = std::env::var("TEST_WEBDAV_PASS").unwrap_or_else(|_| "testpass".into());

    let client = Client::init(&user, &pass, &base_url).expect("client init");
    let cancel_flag = AtomicBool::new(false);
    let mut log_output = String::new();

    // Ensure remote test collection starts clean
    let _ = client.delete(&base_url);
    let _ = client.mkcol(&base_url);

    let tmp_test_dir = std::env::temp_dir().join(format!(
        "scs_inc_test_{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let local_sync_dir = tmp_test_dir.join("local");
    fs::create_dir_all(&local_sync_dir).unwrap();

    let file_a = local_sync_dir.join("doc_a.txt");
    let file_b = local_sync_dir.join("doc_b.txt");
    let file_c = local_sync_dir.join("sub").join("doc_c.txt");
    fs::create_dir_all(file_c.parent().unwrap()).unwrap();

    fs::write(&file_a, "alpha-content").unwrap();
    fs::write(&file_b, "beta-content").unwrap();
    fs::write(&file_c, "gamma-content").unwrap();

    // 1. Initial PUT: all 3 files are brand new -> all 3 must be copied
    log_output.clear();
    let res = execute_webdav_action(
        &client,
        "put",
        &base_url,
        &local_sync_dir,
        &cancel_flag,
        |msg| log_output.push_str(msg),
    );
    assert!(res.is_ok(), "Initial PUT failed: {:?}", res);
    assert!(log_output.contains("Copied: doc_a.txt"), "Must copy doc_a.txt: {}", log_output);
    assert!(log_output.contains("Copied: doc_b.txt"), "Must copy doc_b.txt: {}", log_output);
    assert!(log_output.contains("Copied: sub/doc_c.txt"), "Must copy sub/doc_c.txt: {}", log_output);
    assert!(log_output.contains("3 file(s) to upload"), "Must report 3 to upload: {}", log_output);

    // 2. Immediate Second PUT: no changes -> all 3 files must be skipped as up to date!
    log_output.clear();
    let res2 = execute_webdav_action(
        &client,
        "put",
        &base_url,
        &local_sync_dir,
        &cancel_flag,
        |msg| log_output.push_str(msg),
    );
    assert!(res2.is_ok(), "Second PUT failed: {:?}", res2);
    assert!(log_output.contains("3 file(s) up to date"), "Must report 3 up to date: {}", log_output);
    assert!(log_output.contains("0 file(s) to upload"), "Must report 0 to upload: {}", log_output);
    assert!(!log_output.contains("Copied: doc_a.txt"), "Must not re-copy doc_a: {}", log_output);

    // 3. Dry-run PUT (put-dry): verify dry-run logs skip notices for up to date files
    log_output.clear();
    let dry_res = execute_webdav_action(
        &client,
        "put-dry",
        &base_url,
        &local_sync_dir,
        &cancel_flag,
        |msg| log_output.push_str(msg),
    );
    assert!(dry_res.is_ok(), "Dry-run PUT failed: {:?}", dry_res);
    assert!(log_output.contains("3 file(s) up to date"), "Dry run must detect 3 up to date: {}", log_output);

    // 4. Modify doc_a.txt with SAME size (13 bytes) but NEWER timestamp (simulate editing file without changing length)
    fs::write(&file_a, "ALPHA-CONTENT").unwrap();
    // Explicitly advance modification time by 60 seconds using FileTimes to eliminate flaky CI sleep
    let future_time = std::time::SystemTime::now() + std::time::Duration::from_secs(60);
    let f = std::fs::File::options().write(true).open(&file_a).unwrap();
    f.set_times(std::fs::FileTimes::new().set_modified(future_time)).unwrap();
    drop(f);

    log_output.clear();
    let res3 = execute_webdav_action(
        &client,
        "put",
        &base_url,
        &local_sync_dir,
        &cancel_flag,
        |msg| log_output.push_str(msg),
    );
    assert!(res3.is_ok(), "PUT with newer timestamp failed: {:?}", res3);
    assert!(log_output.contains("1 file(s) to upload"), "Must report exactly 1 to upload: {}", log_output);
    assert!(log_output.contains("2 file(s) up to date"), "Must report 2 up to date: {}", log_output);
    assert!(log_output.contains("Copied: doc_a.txt"), "Must copy modified doc_a: {}", log_output);
    assert!(!log_output.contains("Copied: doc_b.txt"), "Must not copy unchanged doc_b: {}", log_output);
    assert!(!log_output.contains("Copied: sub/doc_c.txt"), "Must not copy unchanged sub/doc_c: {}", log_output);

    // Reset doc_a.txt mtime so it is no longer in the future for subsequent steps
    let f = std::fs::File::options().write(true).open(&file_a).unwrap();
    f.set_times(std::fs::FileTimes::new().set_modified(std::time::SystemTime::now() - std::time::Duration::from_secs(10))).unwrap();
    drop(f);

    // 5. Modify doc_b.txt with DIFFERENT size
    fs::write(&file_b, "beta-content-extended-with-extra-text").unwrap();

    log_output.clear();
    let res4 = execute_webdav_action(
        &client,
        "put",
        &base_url,
        &local_sync_dir,
        &cancel_flag,
        |msg| log_output.push_str(msg),
    );
    assert!(res4.is_ok(), "PUT with size change failed: {:?}", res4);
    assert!(log_output.contains("1 file(s) to upload"), "Must report 1 to upload: {}", log_output);
    assert!(log_output.contains("2 file(s) up to date"), "Must report 2 up to date: {}", log_output);
    assert!(log_output.contains("Copied: doc_b.txt"), "Must copy size-changed doc_b: {}", log_output);

    // 6. Add brand new file doc_d.txt
    let file_d = local_sync_dir.join("doc_d.txt");
    fs::write(&file_d, "delta-content-brand-new").unwrap();

    log_output.clear();
    let res5 = execute_webdav_action(
        &client,
        "put",
        &base_url,
        &local_sync_dir,
        &cancel_flag,
        |msg| log_output.push_str(msg),
    );
    assert!(res5.is_ok(), "PUT with new file failed: {:?}", res5);
    assert!(log_output.contains("1 file(s) to upload"), "Must report 1 to upload: {}", log_output);
    assert!(log_output.contains("3 file(s) up to date"), "Must report 3 up to date: {}", log_output);
    assert!(log_output.contains("Copied: doc_d.txt"), "Must copy brand new doc_d: {}", log_output);

    // Cleanup remote test directory & local files
    let _ = client.delete(&base_url);
    let _ = fs::remove_dir_all(&tmp_test_dir);
}

#[test]
fn test_live_copyparty_incremental_get_timestamp_differentiation() {
    let base_url = match std::env::var("TEST_WEBDAV_URL") {
        Ok(url) => format!("{}/incremental_get_test/", url.trim_end_matches('/')),
        Err(_) => {
            eprintln!("TEST_WEBDAV_URL not set, skipping live incremental get test");
            return;
        }
    };
    let user = std::env::var("TEST_WEBDAV_USER").unwrap_or_else(|_| "testuser".into());
    let pass = std::env::var("TEST_WEBDAV_PASS").unwrap_or_else(|_| "testpass".into());

    let client = Client::init(&user, &pass, &base_url).expect("client init");
    let cancel_flag = AtomicBool::new(false);
    let mut log_output = String::new();

    // Ensure remote test collection starts clean
    let _ = client.delete(&base_url);
    let _ = client.mkcol(&base_url);

    let tmp_test_dir = std::env::temp_dir().join(format!(
        "scs_inc_get_test_{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));

    struct CleanupGuard<'a>(&'a Client, String, std::path::PathBuf);
    impl<'a> Drop for CleanupGuard<'a> {
        fn drop(&mut self) {
            let _ = self.0.delete(&self.1);
            let _ = fs::remove_dir_all(&self.2);
        }
    }
    let _guard = CleanupGuard(&client, base_url.clone(), tmp_test_dir.clone());

    let local_seed_dir = tmp_test_dir.join("seed");
    let local_get_dir = tmp_test_dir.join("download");
    fs::create_dir_all(&local_seed_dir).unwrap();
    fs::create_dir_all(&local_get_dir).unwrap();

    let file_a = local_seed_dir.join("file_a.txt");
    let file_b = local_seed_dir.join("file_b.txt");
    let file_c = local_seed_dir.join("sub").join("file_c.txt");
    fs::create_dir_all(file_c.parent().unwrap()).unwrap();

    fs::write(&file_a, "remote-alpha-data").unwrap();
    fs::write(&file_b, "remote-beta-data").unwrap();
    fs::write(&file_c, "remote-gamma-data").unwrap();

    // Populate remote collection using PUT from seed directory
    let put_res = execute_webdav_action(
        &client,
        "put",
        &base_url,
        &local_seed_dir,
        &cancel_flag,
        |msg| log_output.push_str(msg),
    );
    assert!(put_res.is_ok(), "Seed PUT failed: {:?}", put_res);

    // 1. Initial GET into empty local_get_dir: all 3 files must be downloaded
    log_output.clear();
    let res1 = execute_webdav_action(
        &client,
        "get",
        &base_url,
        &local_get_dir,
        &cancel_flag,
        |msg| log_output.push_str(msg),
    );
    assert!(res1.is_ok(), "Initial GET failed: {:?}", res1);
    assert!(log_output.contains("Downloaded: file_a.txt"), "Must download file_a: {}", log_output);
    assert!(log_output.contains("Downloaded: file_b.txt"), "Must download file_b: {}", log_output);
    assert!(log_output.contains("Downloaded: sub/file_c.txt"), "Must download sub/file_c: {}", log_output);
    assert!(log_output.contains("3 file(s) to download"), "Must report 3 to download: {}", log_output);
    assert!(log_output.contains("0 file(s) up to date"), "Must report 0 up to date initially: {}", log_output);

    // Verify downloaded files exist on disk
    assert!(local_get_dir.join("file_a.txt").exists());
    assert!(local_get_dir.join("file_b.txt").exists());
    assert!(local_get_dir.join("sub").join("file_c.txt").exists());

    // 2. Immediate Second GET: files exist and match remote -> all 3 files must be skipped as up to date!
    log_output.clear();
    let res2 = execute_webdav_action(
        &client,
        "get",
        &base_url,
        &local_get_dir,
        &cancel_flag,
        |msg| log_output.push_str(msg),
    );
    assert!(res2.is_ok(), "Second GET failed: {:?}", res2);
    assert!(log_output.contains("3 file(s) up to date"), "Must report 3 up to date: {}", log_output);
    assert!(log_output.contains("0 file(s) to download"), "Must report 0 to download: {}", log_output);
    assert!(!log_output.contains("Downloaded: file_a.txt"), "Must not redownload file_a: {}", log_output);

    // 3. Dry-run GET (get-dry): verify dry-run logs skip notices for up to date files
    log_output.clear();
    let dry_res = execute_webdav_action(
        &client,
        "get-dry",
        &base_url,
        &local_get_dir,
        &cancel_flag,
        |msg| log_output.push_str(msg),
    );
    assert!(dry_res.is_ok(), "Dry-run GET failed: {:?}", dry_res);
    assert!(log_output.contains("3 file(s) up to date"), "Dry run must detect 3 up to date: {}", log_output);

    // 4. Modify remote file_b.txt with DIFFERENT size and upload to remote
    fs::write(&file_b, "remote-beta-data-extended-content-size-change").unwrap();
    let put_mod = execute_webdav_action(
        &client,
        "put",
        &base_url,
        &local_seed_dir,
        &cancel_flag,
        |_| {},
    );
    assert!(put_mod.is_ok(), "PUT modified seed failed: {:?}", put_mod);

    log_output.clear();
    let res3 = execute_webdav_action(
        &client,
        "get",
        &base_url,
        &local_get_dir,
        &cancel_flag,
        |msg| log_output.push_str(msg),
    );
    assert!(res3.is_ok(), "GET with size change failed: {:?}", res3);
    assert!(log_output.contains("1 file(s) to download"), "Must report 1 to download: {}", log_output);
    assert!(log_output.contains("2 file(s) up to date"), "Must report 2 up to date: {}", log_output);
    assert!(log_output.contains("Downloaded: file_b.txt"), "Must download modified file_b: {}", log_output);
    assert!(!log_output.contains("Downloaded: file_a.txt"), "Must not download unchanged file_a: {}", log_output);
    assert!(!log_output.contains("Downloaded: sub/file_c.txt"), "Must not download unchanged sub/file_c: {}", log_output);

    // 5. Add brand new file file_d.txt to remote
    let file_d = local_seed_dir.join("file_d.txt");
    fs::write(&file_d, "brand-new-remote-file-d").unwrap();
    let put_new = execute_webdav_action(
        &client,
        "put",
        &base_url,
        &local_seed_dir,
        &cancel_flag,
        |_| {},
    );
    assert!(put_new.is_ok(), "PUT new seed failed: {:?}", put_new);

    // Test GET with parallel scan concurrency
    log_output.clear();
    let res4 = scs_rclient_lib::webdav::execute_webdav_action_with_options(
        &client,
        "get",
        &base_url,
        &local_get_dir,
        &cancel_flag,
        Some(4),
        Some(&user),
        |msg| log_output.push_str(msg),
    );
    assert!(res4.is_ok(), "GET with concurrency option failed: {:?}", res4);
    assert!(log_output.contains("1 file(s) to download"), "Must report 1 to download: {}", log_output);
    assert!(log_output.contains("3 file(s) up to date"), "Must report 3 up to date: {}", log_output);
    assert!(log_output.contains("Downloaded: file_d.txt"), "Must download brand new file_d: {}", log_output);

    // Cleanup remote test directory & local files
    let _ = client.delete(&base_url);
    let _ = fs::remove_dir_all(&tmp_test_dir);
}

#[test]
fn test_concurrent_scanning_stress_1_to_64_threads() {
    use std::collections::HashMap;
    use std::io::{Read, Write};
    use std::net::{TcpListener, TcpStream};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;

    assert_eq!(scs_rclient_lib::webdav::MAX_SCAN_CONCURRENCY, 64);

    // Build ample directory tree: 10 modules with nested folders, deep archives, and shared assets
    struct MockEntry {
        subfolders: Vec<String>,
        files: Vec<(String, u64)>, // (filename, size)
    }

    let mut dir_map: HashMap<String, MockEntry> = HashMap::new();

    // Root collection
    let root_path = "/stress-course/".to_string();
    let mut root_subfolders = Vec::new();
    let root_files = vec![
        ("Syllabus.pdf".to_string(), 102400),
        ("Course_Schedule.xlsx".to_string(), 51200),
        ("README.txt".to_string(), 1024),
    ];

    let mut total_expected_dirs = 0;
    let mut total_expected_files = root_files.len();

    // 10 modules with diverse nested directory structures
    for m in 1..=10 {
        let mod_name = format!("Module_{:02}", m);
        root_subfolders.push(format!("{}/", mod_name));
        total_expected_dirs += 1;

        let mod_path = format!("/stress-course/{}/", mod_name);
        let mod_subfolders = vec![
            "Lectures/".to_string(),
            "Assignments/".to_string(),
            "Readings/".to_string(),
            "Assets/".to_string(),
        ];
        total_expected_dirs += 4;
        let mod_files = vec![
            (format!("overview_m{:02}.pdf", m), 20480),
            (format!("goals_m{:02}.txt", m), 2048),
        ];
        total_expected_files += mod_files.len();
        dir_map.insert(
            mod_path,
            MockEntry {
                subfolders: mod_subfolders,
                files: mod_files,
            },
        );

        // Lectures
        let lec_path = format!("/stress-course/{}/Lectures/", mod_name);
        let lec_files = vec![
            ("slides.pdf".to_string(), 150000),
            ("notes.md".to_string(), 5000),
            ("transcript.vtt".to_string(), 12000),
        ];
        total_expected_files += lec_files.len();
        dir_map.insert(
            lec_path,
            MockEntry {
                subfolders: Vec::new(),
                files: lec_files,
            },
        );

        // Assignments
        let assn_path = format!("/stress-course/{}/Assignments/", mod_name);
        let assn_files = vec![
            ("rubric.pdf".to_string(), 35000),
            ("spec.docx".to_string(), 45000),
            ("solution_sample.zip".to_string(), 250000),
        ];
        total_expected_files += assn_files.len();
        dir_map.insert(
            assn_path,
            MockEntry {
                subfolders: Vec::new(),
                files: assn_files,
            },
        );

        // Readings
        let read_path = format!("/stress-course/{}/Readings/", mod_name);
        let read_files = vec![
            ("paper1.pdf".to_string(), 85000),
            ("paper2.pdf".to_string(), 92000),
        ];
        total_expected_files += read_files.len();
        dir_map.insert(
            read_path,
            MockEntry {
                subfolders: Vec::new(),
                files: read_files,
            },
        );

        // Assets with nested Images
        let assets_path = format!("/stress-course/{}/Assets/", mod_name);
        total_expected_dirs += 1;
        dir_map.insert(
            assets_path,
            MockEntry {
                subfolders: vec!["Images/".to_string()],
                files: vec![("data.csv".to_string(), 4000)],
            },
        );
        total_expected_files += 1;

        let img_path = format!("/stress-course/{}/Assets/Images/", mod_name);
        let img_files = vec![
            ("diagram.png".to_string(), 30000),
            ("chart.svg".to_string(), 15000),
        ];
        total_expected_files += img_files.len();
        dir_map.insert(
            img_path,
            MockEntry {
                subfolders: Vec::new(),
                files: img_files,
            },
        );
    }

    // Deep nested archives
    root_subfolders.push("Archives/".to_string());
    total_expected_dirs += 6;
    dir_map.insert(
        "/stress-course/Archives/".to_string(),
        MockEntry {
            subfolders: vec!["2024/".to_string()],
            files: Vec::new(),
        },
    );
    dir_map.insert(
        "/stress-course/Archives/2024/".to_string(),
        MockEntry {
            subfolders: vec!["Winter/".to_string()],
            files: Vec::new(),
        },
    );
    dir_map.insert(
        "/stress-course/Archives/2024/Winter/".to_string(),
        MockEntry {
            subfolders: vec!["Midterms/".to_string()],
            files: Vec::new(),
        },
    );
    dir_map.insert(
        "/stress-course/Archives/2024/Winter/Midterms/".to_string(),
        MockEntry {
            subfolders: vec!["Solutions/".to_string()],
            files: Vec::new(),
        },
    );
    dir_map.insert(
        "/stress-course/Archives/2024/Winter/Midterms/Solutions/".to_string(),
        MockEntry {
            subfolders: vec!["V1/".to_string()],
            files: vec![("exam_master.pdf".to_string(), 80000)],
        },
    );
    total_expected_files += 1;
    dir_map.insert(
        "/stress-course/Archives/2024/Winter/Midterms/Solutions/V1/".to_string(),
        MockEntry {
            subfolders: Vec::new(),
            files: vec![
                ("q1_sol.pdf".to_string(), 25000),
                ("q2_sol.pdf".to_string(), 30000),
            ],
        },
    );
    total_expected_files += 2;

    // Common shared directory
    root_subfolders.push("Shared/".to_string());
    total_expected_dirs += 3;
    dir_map.insert(
        "/stress-course/Shared/".to_string(),
        MockEntry {
            subfolders: vec!["Code/".to_string()],
            files: Vec::new(),
        },
    );
    dir_map.insert(
        "/stress-course/Shared/Code/".to_string(),
        MockEntry {
            subfolders: vec!["Python/".to_string()],
            files: Vec::new(),
        },
    );
    dir_map.insert(
        "/stress-course/Shared/Code/Python/".to_string(),
        MockEntry {
            subfolders: Vec::new(),
            files: vec![
                ("main.py".to_string(), 1200),
                ("test.py".to_string(), 800),
                ("config.json".to_string(), 350),
            ],
        },
    );
    total_expected_files += 3;

    dir_map.insert(
        root_path,
        MockEntry {
            subfolders: root_subfolders,
            files: root_files,
        },
    );

    // Pre-generate WebDAV XML Multi-Status responses for instant serving
    let mut responses: HashMap<String, Vec<u8>> = HashMap::new();
    for (dir_path, entry) in &dir_map {
        let mut xml = String::new();
        xml.push_str(r#"<?xml version="1.0" encoding="utf-8"?><D:multistatus xmlns:D="DAV:">"#);
        xml.push_str(&format!(
            r#"<D:response><D:href>{}</D:href><D:propstat><D:prop><D:resourcetype><D:collection/></D:resourcetype></D:prop><D:status>HTTP/1.1 200 OK</D:status></D:propstat></D:response>"#,
            dir_path
        ));
        for sub in &entry.subfolders {
            let full_sub = format!("{}{}", dir_path, sub);
            xml.push_str(&format!(
                r#"<D:response><D:href>{}</D:href><D:propstat><D:prop><D:resourcetype><D:collection/></D:resourcetype></D:prop><D:status>HTTP/1.1 200 OK</D:status></D:propstat></D:response>"#,
                full_sub
            ));
        }
        for (f, sz) in &entry.files {
            let full_f = format!("{}{}", dir_path, f);
            xml.push_str(&format!(
                r#"<D:response><D:href>{}</D:href><D:propstat><D:prop><D:resourcetype/><D:getcontentlength>{}</D:getcontentlength><D:getlastmodified>Mon, 28 Sep 2026 12:00:00 GMT</D:getlastmodified></D:prop><D:status>HTTP/1.1 200 OK</D:status></D:propstat></D:response>"#,
                full_f, sz
            ));
        }
        xml.push_str("</D:multistatus>");

        let resp = format!(
            "HTTP/1.1 207 Multi-Status\r\nContent-Type: application/xml; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            xml.len(),
            xml
        );
        responses.insert(dir_path.trim_end_matches('/').to_ascii_lowercase(), resp.into_bytes());
    }

    let shared_responses = Arc::new(responses);

    // Spawn mock WebDAV server that simulates D2L/IIS Depth: infinity rejection
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let is_running = Arc::new(AtomicBool::new(true));

    let server_running = is_running.clone();
    let server_resp = shared_responses.clone();

    let server_handle = std::thread::spawn(move || {
        while server_running.load(Ordering::SeqCst) {
            let (mut stream, _) = match listener.accept() {
                Ok(conn) => conn,
                Err(_) => break,
            };
            if !server_running.load(Ordering::SeqCst) {
                break;
            }

            let resp_map = server_resp.clone();
            std::thread::spawn(move || {
                let mut buf = [0u8; 4096];
                let n = match stream.read(&mut buf) {
                    Ok(n) if n > 0 => n,
                    _ => return,
                };
                let req_str = String::from_utf8_lossy(&buf[..n]);

                // Simulate D2L/IIS rejection of Depth: infinity with HTTP 403 Forbidden
                if req_str.to_ascii_lowercase().contains("depth: infinity") {
                    let forbidden = "HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
                    let _ = stream.write_all(forbidden.as_bytes());
                    return;
                }

                let first_line = req_str.lines().next().unwrap_or("");
                let path = first_line.split_whitespace().nth(1).unwrap_or("/");
                let lookup_key = path.trim_end_matches('/').to_ascii_lowercase();

                if let Some(resp_bytes) = resp_map.get(&lookup_key) {
                    let _ = stream.write_all(resp_bytes);
                } else {
                    let not_found = "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
                    let _ = stream.write_all(not_found.as_bytes());
                }
            });
        }
    });

    let cancel_flag = AtomicBool::new(false);
    let base_url = format!("http://127.0.0.1:{}/stress-course/", port);
    let client = Client::init("testuser", "testpass", &base_url).expect("client init");

    let total_expected_items = total_expected_files + total_expected_dirs;
    assert_eq!(total_expected_dirs, 69);
    assert_eq!(total_expected_files, 139);
    assert_eq!(total_expected_items, 208);

    // Stress test across all worker thread counts from 1 to 64
    for threads in 1..=64 {
        let items = scs_rclient_lib::webdav::list_remote_recursive_with_concurrency_and_log(
            &client,
            &base_url,
            &cancel_flag,
            Some(threads),
            Some("testuser"),
            |_| {},
        )
        .unwrap_or_else(|e| panic!("Concurrent scan failed with {} threads: {}", threads, e));

        let files_count = items.iter().filter(|i| !i.is_dir).count();
        let dirs_count = items.iter().filter(|i| i.is_dir).count();

        assert_eq!(
            files_count, total_expected_files,
            "Thread count {} found {} files, expected {}",
            threads, files_count, total_expected_files
        );
        assert_eq!(
            dirs_count, total_expected_dirs,
            "Thread count {} found {} dirs, expected {}",
            threads, dirs_count, total_expected_dirs
        );
        assert_eq!(
            items.len(),
            total_expected_items,
            "Thread count {} found {} items, expected {}",
            threads,
            items.len(),
            total_expected_items
        );
    }

    // Cleanly stop mock server
    is_running.store(false, Ordering::SeqCst);
    let _ = TcpStream::connect(format!("127.0.0.1:{}", port));
    let _ = server_handle.join();
}

#[test]
fn test_live_copyparty_excluded_folders_are_not_scanned_or_transferred() {
    let base_url = match std::env::var("TEST_WEBDAV_URL") {
        Ok(v) if !v.trim().is_empty() => v,
        _ => {
            eprintln!(
                "SKIPPED: TEST_WEBDAV_URL not set; skipping live excluded-folder test."
            );
            return;
        }
    };
    let username = std::env::var("TEST_WEBDAV_USER").unwrap_or_else(|_| "testuser".to_string());
    let password = std::env::var("TEST_WEBDAV_PASS").unwrap_or_else(|_| "testpass".to_string());
    let client = Client::init(&username, &password, &base_url)
        .expect("client init");
    let cancel_flag = AtomicBool::new(false);
    let mut log_output = String::new();

    // Dedicated collection so the test never disturbs the other live tests.
    let test_url = format!("{}/excluded_test/", base_url.trim_end_matches('/'));
    let _ = client.delete(&test_url);
    let _ = client.mkcol(&test_url);

    let temp_root = std::env::temp_dir().join(format!(
        "scs_exclude_e2e_{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let seed_dir = temp_root.join("seed");
    let project_dir = temp_root.join("project");
    let download_dir = temp_root.join("download");

    let write = |root: &std::path::Path, rel: &str, contents: &[u8]| {
        let path = root.join(rel);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, contents).unwrap();
    };

    // Seed remote content that lives inside the folders we are about to exclude, using a
    // normal PUT so it is genuinely present on the server before exclusions take effect.
    write(&seed_dir, ".git/config", b"seeded git config");
    write(&seed_dir, "build/cache/artifact.bin", b"seeded artifact");
    let seed_res = execute_webdav_action(
        &client,
        "put",
        &test_url,
        &seed_dir,
        &cancel_flag,
        |_| {},
    );
    assert!(
        seed_res.is_ok(),
        "Seeding remote excluded folders failed: {:?}",
        seed_res
    );

    // Local working copy contains both excluded and included content.
    write(&project_dir, "keep.txt", b"keep");
    write(&project_dir, "src/main.rs", b"fn main() {}");
    write(&project_dir, ".git/config", b"local git config");
    write(&project_dir, "build/cache/artifact.bin", b"local artifact");

    let exclusions = normalize_exclude_folders(&[".git/".to_string(), "build/cache".to_string()]);

    // 1. Upload: excluded local folders must not be copied, and the remote scan must not
    //    descend into them (the scan log below proves they were never requested).
    log_output.clear();
    let put_res = execute_webdav_action_with_exclusions(
        &client,
        "put",
        &test_url,
        &project_dir,
        &cancel_flag,
        None,
        None,
        &exclusions,
        None,
        |msg| log_output.push_str(msg),
    );
    assert!(put_res.is_ok(), "PUT with exclusions failed: {:?}", put_res);
    assert!(
        log_output.contains("Copied: keep.txt"),
        "keep.txt must upload: {}",
        log_output
    );
    assert!(
        log_output.contains("Copied: src/main.rs"),
        "src/main.rs must upload: {}",
        log_output
    );
    assert!(
        !log_output.contains("Copied: .git/config"),
        ".git must not upload: {}",
        log_output
    );
    assert!(
        !log_output.contains("Copied: build/cache/artifact.bin"),
        "build/cache must not upload: {}",
        log_output
    );
    assert!(
        !log_output.contains("Scanning directory: .git"),
        ".git must not be scanned: {}",
        log_output
    );
    assert!(
        !log_output.contains("Scanning directory: build/cache"),
        "build/cache must not be scanned: {}",
        log_output
    );

    // The seeded content still exists on the remote even though the pruned scan never saw it.
    let remote_paths = |url: &str| -> HashSet<String> {
        list_remote_recursive(&client, url, &cancel_flag)
            .unwrap()
            .iter()
            .filter(|item| !item.is_dir)
            .map(|item| relative_item_path(url, &item.href))
            .collect()
    };
    let seeded = remote_paths(&test_url);
    assert!(
        seeded.contains(".git/config"),
        "seeded .git/config must exist remotely: {:?}",
        seeded
    );
    assert!(
        seeded.contains("build/cache/artifact.bin"),
        "seeded build/cache must exist remotely: {:?}",
        seeded
    );
    assert!(seeded.contains("keep.txt"), "keep.txt must exist remotely: {:?}", seeded);
    assert!(seeded.contains("src/main.rs"), "src/main.rs must exist remotely: {:?}", seeded);

    // 2. Download into an empty directory: excluded remote folders must be skipped.
    fs::create_dir_all(&download_dir).unwrap();
    log_output.clear();
    let get_res = execute_webdav_action_with_exclusions(
        &client,
        "get",
        &test_url,
        &download_dir,
        &cancel_flag,
        None,
        None,
        &exclusions,
        None,
        |msg| log_output.push_str(msg),
    );
    assert!(get_res.is_ok(), "GET with exclusions failed: {:?}", get_res);
    assert!(
        log_output.contains("Downloaded: keep.txt"),
        "keep.txt must download: {}",
        log_output
    );
    assert!(
        log_output.contains("Downloaded: src/main.rs"),
        "src/main.rs must download: {}",
        log_output
    );
    assert!(
        !log_output.contains("Downloaded: .git/config"),
        ".git must not download: {}",
        log_output
    );
    assert!(
        !log_output.contains("Downloaded: build/cache/artifact.bin"),
        "build/cache must not download: {}",
        log_output
    );
    assert!(
        !download_dir.join(".git").exists(),
        ".git must not be created locally"
    );
    assert!(
        !download_dir.join("build").join("cache").exists(),
        "build/cache must not be created locally"
    );
    let mut downloaded: Vec<String> =
        collect_local_files_excluding(&download_dir, &exclusions, None)
        .into_iter()
        .map(|(_, rel, _)| rel)
        .collect();
    downloaded.sort();
    assert_eq!(
        downloaded,
        vec!["keep.txt".to_string(), "src/main.rs".to_string()]
    );

    // 3. Sync must not delete remote files inside excluded folders.
    fs::remove_file(download_dir.join("src/main.rs")).unwrap();
    log_output.clear();
    let sync_res = execute_webdav_action_with_exclusions(
        &client,
        "sync",
        &test_url,
        &download_dir,
        &cancel_flag,
        None,
        None,
        &exclusions,
        None,
        |msg| log_output.push_str(msg),
    );
    assert!(sync_res.is_ok(), "SYNC with exclusions failed: {:?}", sync_res);
    assert!(
        log_output.contains("Deleted remote file not in local: src/main.rs"),
        "sync should delete src/main.rs: {}",
        log_output
    );
    assert!(
        !log_output.contains("Deleted remote file not in local: .git/config"),
        "sync must not delete excluded .git/config: {}",
        log_output
    );
    assert!(
        !log_output.contains("Deleted remote file not in local: build/cache/artifact.bin"),
        "sync must not delete excluded build/cache: {}",
        log_output
    );
    let after_sync = remote_paths(&test_url);
    assert!(
        after_sync.contains(".git/config"),
        ".git/config must survive sync: {:?}",
        after_sync
    );
    assert!(
        after_sync.contains("build/cache/artifact.bin"),
        "build/cache must survive sync: {:?}",
        after_sync
    );
    assert!(
        !after_sync.contains("src/main.rs"),
        "src/main.rs should be deleted: {:?}",
        after_sync
    );

    // Cleanup: remove the dedicated remote collection and local temp tree.
    if let Ok(items) = list_remote_recursive(&client, &test_url, &cancel_flag) {
        for item in items {
            let rel = relative_item_path(&test_url, &item.href);
            if !rel.is_empty() {
                let _ = client.delete(&resolve_item_url(&test_url, &item.href));
            }
        }
    }
    let _ = client.delete(&test_url);
    let _ = fs::remove_dir_all(&temp_root);
}

/// End-to-end coverage for nested `.git` folders, name-vs-path patterns, and near misses.
///
/// Seeds excluded folders on the server first, then runs PUT/GET/SYNC with exclusions and
/// checks three things each time: the excluded content is not transferred, the scan log
/// never visits it, and the seeded remote copies are left byte-for-byte intact.
#[test]
fn test_live_copyparty_excluded_folders_nested_and_path_patterns() {
    let base_url = match live_base_url() {
        Some(url) => url,
        None => {
            eprintln!(
                "SKIPPED: TEST_WEBDAV_URL not set; skipping nested excluded-folder e2e test."
            );
            return;
        }
    };
    let client = live_client();
    let cancel_flag = AtomicBool::new(false);
    let mut log_output = String::new();

    let test_url = format!("{}/excluded_nested_test/", base_url.trim_end_matches('/'));
    remove_remote_tree(&client, &test_url, &cancel_flag);
    let _ = client.mkcol(&test_url);

    let temp_root = std::env::temp_dir().join(format!(
        "scs_exclude_nested_{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let seed_dir = temp_root.join("seed");
    let project_dir = temp_root.join("project");
    let download_dir = temp_root.join("download");

    // Seed every excluded folder on the server with sentinel content, plus the included
    // files that surround them.
    for (rel, contents) in [
        (".git/config", "seed-root-git"),
        (".gitlab/notes.txt", "seed-gitlab"),
        ("node_modules/pkg/index.js", "seed-node-modules"),
        ("assets/cache/data.bin", "seed-cache"),
        ("assets/cache2/keep.txt", "seed-cache2"),
        ("assets/keep.txt", "seed-assets-keep"),
        ("module/.git/config", "seed-module-git"),
        ("module/deep/.git/config", "seed-deep-git"),
        ("module/deep/keep.txt", "seed-deep-keep"),
        ("module/notes.txt", "seed-module-notes"),
        ("root-keep.txt", "seed-root-keep"),
        ("README.md", "seed-readme"),
    ] {
        write_local_file(&seed_dir, rel, contents.as_bytes());
    }
    let seed_res = execute_webdav_action(
        &client,
        "put",
        &test_url,
        &seed_dir,
        &cancel_flag,
        |_| {},
    );
    assert!(
        seed_res.is_ok(),
        "Seeding remote excluded folders failed: {:?}",
        seed_res
    );

    let included_paths = [
        ".gitlab/notes.txt",
        "README.md",
        "assets/cache2/keep.txt",
        "assets/keep.txt",
        "module/deep/keep.txt",
        "module/new.txt",
        "module/notes.txt",
        "root-keep.txt",
    ];
    let excluded_paths = [
        ".git/config",
        "assets/cache/data.bin",
        "module/.git/config",
        "module/deep/.git/config",
        "node_modules/pkg/index.js",
    ];

    // Local working copy: the same tree with different content, so a successful upload is
    // observable, plus one brand-new included file.
    for (rel, contents) in [
        (".git/config", "project-root-git"),
        (".gitlab/notes.txt", "project-gitlab"),
        ("node_modules/pkg/index.js", "project-node-modules"),
        ("assets/cache/data.bin", "project-cache"),
        ("assets/cache2/keep.txt", "project-cache2"),
        ("assets/keep.txt", "project-assets-keep"),
        ("module/.git/config", "project-module-git"),
        ("module/deep/.git/config", "project-deep-git"),
        ("module/deep/keep.txt", "project-deep-keep"),
        ("module/new.txt", "project-new"),
        ("module/notes.txt", "project-module-notes"),
        ("root-keep.txt", "project-root-keep"),
        ("README.md", "project-readme"),
    ] {
        write_local_file(&project_dir, rel, contents.as_bytes());
    }

    let exclusions = normalize_exclude_folders(&[
        ".git/".to_string(),
        "node_modules".to_string(),
        "assets/cache".to_string(),
    ]);

    // 1. PUT: included files upload; root, one-level, and two-level `.git` folders are skipped.
    log_output.clear();
    let put_res = execute_webdav_action_with_exclusions(
        &client,
        "put",
        &test_url,
        &project_dir,
        &cancel_flag,
        None,
        None,
        &exclusions,
        None,
        |msg| log_output.push_str(msg),
    );
    assert!(put_res.is_ok(), "PUT with exclusions failed: {:?}", put_res);
    for included in included_paths {
        assert!(
            log_output.contains(&format!("Copied: {included}")),
            "{} must upload: {}",
            included,
            log_output
        );
    }
    for excluded in excluded_paths {
        assert!(
            !log_output.contains(&format!("Copied: {excluded}")),
            "{} must not upload: {}",
            excluded,
            log_output
        );
    }
    assert_not_scanned_directories(
        &log_output,
        &[
            ".git",
            "module/.git",
            "module/deep/.git",
            "node_modules",
            "assets/cache",
        ],
    );
    if !scanned_directories(&log_output).is_empty() {
        assert_scanned_directories(
            &log_output,
            &[".gitlab", "assets", "assets/cache2", "module", "module/deep"],
        );
    }

    // The seeded copies inside excluded folders must be untouched by the upload.
    for (rel, expected) in [
        (".git/config", "seed-root-git"),
        ("module/.git/config", "seed-module-git"),
        ("module/deep/.git/config", "seed-deep-git"),
        ("node_modules/pkg/index.js", "seed-node-modules"),
        ("assets/cache/data.bin", "seed-cache"),
        (".gitlab/notes.txt", "project-gitlab"),
        ("assets/cache2/keep.txt", "project-cache2"),
    ] {
        assert_eq!(
            read_remote_text(&client, &build_file_url(&test_url, rel)),
            expected,
            "remote content mismatch for {}",
            rel
        );
    }
    let after_put = remote_file_paths(&client, &test_url, &cancel_flag);
    for excluded in excluded_paths {
        assert!(
            after_put.contains(excluded),
            "seeded excluded file {} must still exist remotely: {:?}",
            excluded,
            after_put
        );
    }
    for near_miss in [".gitlab/notes.txt", "assets/cache2/keep.txt"] {
        assert!(
            after_put.contains(near_miss),
            "{} must not be excluded: {:?}",
            near_miss,
            after_put
        );
    }

    // 2. GET: excluded remote folders must not create anything locally, at any depth.
    fs::create_dir_all(&download_dir).unwrap();
    log_output.clear();
    let get_res = execute_webdav_action_with_exclusions(
        &client,
        "get",
        &test_url,
        &download_dir,
        &cancel_flag,
        None,
        None,
        &exclusions,
        None,
        |msg| log_output.push_str(msg),
    );
    assert!(get_res.is_ok(), "GET with exclusions failed: {:?}", get_res);
    for included in included_paths {
        assert!(
            log_output.contains(&format!("Downloaded: {included}")),
            "{} must download: {}",
            included,
            log_output
        );
    }
    for excluded in excluded_paths {
        assert!(
            !log_output.contains(&format!("Downloaded: {excluded}")),
            "{} must not download: {}",
            excluded,
            log_output
        );
    }
    assert_eq!(
        local_file_paths(&download_dir),
        included_paths
            .iter()
            .map(|path| path.to_string())
            .collect::<Vec<String>>(),
        "GET must create exactly the included files"
    );
    for excluded_dir in [
        ".git",
        "node_modules",
        "assets/cache",
        "module/.git",
        "module/deep/.git",
    ] {
        assert!(
            !download_dir.join(excluded_dir).exists(),
            "excluded folder {} must not be created locally",
            excluded_dir
        );
    }

    // 3. SYNC: a missing included file is deleted remotely, excluded folders are not.
    fs::remove_file(download_dir.join("README.md")).unwrap();
    log_output.clear();
    let sync_res = execute_webdav_action_with_exclusions(
        &client,
        "sync",
        &test_url,
        &download_dir,
        &cancel_flag,
        None,
        None,
        &exclusions,
        None,
        |msg| log_output.push_str(msg),
    );
    assert!(sync_res.is_ok(), "SYNC with exclusions failed: {:?}", sync_res);
    assert!(
        log_output.contains("Deleted remote file not in local: README.md"),
        "sync should delete README.md: {}",
        log_output
    );
    for excluded in excluded_paths {
        assert!(
            !log_output.contains(&format!("Deleted remote file not in local: {excluded}")),
            "sync must not delete excluded {}: {}",
            excluded,
            log_output
        );
    }
    let after_sync = remote_file_paths(&client, &test_url, &cancel_flag);
    assert!(
        !after_sync.contains("README.md"),
        "README.md should be deleted: {:?}",
        after_sync
    );
    for kept in excluded_paths {
        assert!(
            after_sync.contains(kept),
            "excluded {} must survive sync: {:?}",
            kept,
            after_sync
        );
    }
    for near_miss in [".gitlab/notes.txt", "assets/cache2/keep.txt"] {
        assert!(
            after_sync.contains(near_miss),
            "{} must survive sync: {:?}",
            near_miss,
            after_sync
        );
    }

    remove_remote_tree(&client, &test_url, &cancel_flag);
    let _ = fs::remove_dir_all(&temp_root);
}

/// End-to-end coverage for a `.git` folder one level above the scan root ("current dir").
///
/// The parent collection also holds ordinary files and a `.git` folder. Actions are scoped
/// to `current/`, so the parent `.git` must never be scanned, downloaded, overwritten, or
/// deleted — and the scan must still exclude `.git` inside `current/` itself.
#[test]
fn test_live_copyparty_excluded_folder_above_scan_root_is_untouched() {
    let base_url = match live_base_url() {
        Some(url) => url,
        None => {
            eprintln!(
                "SKIPPED: TEST_WEBDAV_URL not set; skipping above-root excluded-folder e2e test."
            );
            return;
        }
    };
    let client = live_client();
    let cancel_flag = AtomicBool::new(false);
    let mut log_output = String::new();

    let test_url = format!("{}/excluded_above_test/", base_url.trim_end_matches('/'));
    let current_url = format!("{}current/", test_url);
    remove_remote_tree(&client, &test_url, &cancel_flag);
    let _ = client.mkcol(&test_url);

    let temp_root = std::env::temp_dir().join(format!(
        "scs_exclude_above_{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let seed_dir = temp_root.join("seed");
    let local_parent = temp_root.join("local");
    let download_parent = temp_root.join("download");

    // Seed the parent AND the current directory on the server.
    for (rel, contents) in [
        (".git/config", "seed-parent-git"),
        ("parent-keep.txt", "seed-parent-keep"),
        ("current/.git/config", "seed-current-git"),
        ("current/nested/.git/config", "seed-nested-git"),
        ("current/keep.txt", "seed-current-keep"),
        ("current/nested/keep.txt", "seed-nested-keep"),
    ] {
        write_local_file(&seed_dir, rel, contents.as_bytes());
    }
    let seed_res = execute_webdav_action(
        &client,
        "put",
        &test_url,
        &seed_dir,
        &cancel_flag,
        |_| {},
    );
    assert!(
        seed_res.is_ok(),
        "Seeding remote above-root tree failed: {:?}",
        seed_res
    );

    // The local working copy for the scoped action lives at local/current/. Its parent
    // contains sentinel `.git` content that must remain untouched.
    write_local_file(&local_parent, ".git/config", b"local-parent-git");
    write_local_file(&local_parent, "parent-keep.txt", b"local-parent-keep");
    write_local_file(&local_parent, "current/keep.txt", b"local-current-keep");
    write_local_file(&local_parent, "current/extra.txt", b"local-current-extra");
    write_local_file(&local_parent, "current/.git/config", b"local-current-git");
    write_local_file(&local_parent, "current/nested/keep.txt", b"local-nested-keep");
    write_local_file(
        &local_parent,
        "current/nested/.git/config",
        b"local-nested-git",
    );

    let exclusions = normalize_exclude_folders(&[".git/".to_string()]);

    // 1. PUT scoped to current/: only current/'s included files upload.
    log_output.clear();
    let put_res = execute_webdav_action_with_exclusions(
        &client,
        "put",
        &current_url,
        &local_parent.join("current"),
        &cancel_flag,
        None,
        None,
        &exclusions,
        None,
        |msg| log_output.push_str(msg),
    );
    assert!(put_res.is_ok(), "PUT above-root test failed: {:?}", put_res);
    for included in ["extra.txt", "keep.txt", "nested/keep.txt"] {
        assert!(
            log_output.contains(&format!("Copied: {included}")),
            "{} must upload: {}",
            included,
            log_output
        );
    }
    for excluded in [".git/config", "nested/.git/config"] {
        assert!(
            !log_output.contains(&format!("Copied: {excluded}")),
            "{} must not upload: {}",
            excluded,
            log_output
        );
    }
    assert_not_scanned_directories(&log_output, &[".git", "nested/.git"]);
    if !scanned_directories(&log_output).is_empty() {
        assert_scanned_directories(&log_output, &["nested"]);
    }

    // Parent-level content and excluded current-level content are untouched.
    for (rel, expected) in [
        (".git/config", "seed-parent-git"),
        ("parent-keep.txt", "seed-parent-keep"),
        ("current/.git/config", "seed-current-git"),
        ("current/nested/.git/config", "seed-nested-git"),
        ("current/keep.txt", "local-current-keep"),
    ] {
        assert_eq!(
            read_remote_text(&client, &build_file_url(&test_url, rel)),
            expected,
            "remote content mismatch for {}",
            rel
        );
    }

    // 2. GET scoped to current/: the parent's local `.git` and keep file are not touched.
    write_local_file(
        &download_parent,
        ".git/config",
        b"download-parent-git",
    );
    write_local_file(
        &download_parent,
        "parent-keep.txt",
        b"download-parent-keep",
    );
    fs::create_dir_all(download_parent.join("current")).unwrap();
    log_output.clear();
    let get_res = execute_webdav_action_with_exclusions(
        &client,
        "get",
        &current_url,
        &download_parent.join("current"),
        &cancel_flag,
        None,
        None,
        &exclusions,
        None,
        |msg| log_output.push_str(msg),
    );
    assert!(get_res.is_ok(), "GET above-root test failed: {:?}", get_res);
    assert_eq!(
        local_file_paths(&download_parent.join("current")),
        vec![
            "extra.txt".to_string(),
            "keep.txt".to_string(),
            "nested/keep.txt".to_string(),
        ],
        "GET must download only current/ included files"
    );
    assert!(
        !download_parent.join("current/.git").exists(),
        "current/.git must not be created locally"
    );
    assert!(
        !download_parent.join("current/nested/.git").exists(),
        "current/nested/.git must not be created locally"
    );
    assert_eq!(
        fs::read_to_string(download_parent.join(".git/config")).unwrap(),
        "download-parent-git",
        "parent .git must not be overwritten by a scoped GET"
    );
    assert_eq!(
        fs::read_to_string(download_parent.join("parent-keep.txt")).unwrap(),
        "download-parent-keep",
        "parent keep file must not be touched by a scoped GET"
    );

    // 3. SYNC scoped to current/: deletes only current/keep.txt, never parent content.
    fs::remove_file(download_parent.join("current/keep.txt")).unwrap();
    log_output.clear();
    let sync_res = execute_webdav_action_with_exclusions(
        &client,
        "sync",
        &current_url,
        &download_parent.join("current"),
        &cancel_flag,
        None,
        None,
        &exclusions,
        None,
        |msg| log_output.push_str(msg),
    );
    assert!(sync_res.is_ok(), "SYNC above-root test failed: {:?}", sync_res);
    assert!(
        log_output.contains("Deleted remote file not in local: keep.txt"),
        "sync should delete current/keep.txt: {}",
        log_output
    );
    for excluded in [".git/config", "nested/.git/config"] {
        assert!(
            !log_output.contains(&format!("Deleted remote file not in local: {excluded}")),
            "sync must not delete excluded {}: {}",
            excluded,
            log_output
        );
    }

    let after_current = remote_file_paths(&client, &current_url, &cancel_flag);
    assert!(
        !after_current.contains("keep.txt"),
        "current/keep.txt should be deleted: {:?}",
        after_current
    );
    assert!(
        after_current.contains(".git/config"),
        "current/.git/config must survive sync: {:?}",
        after_current
    );
    assert!(
        after_current.contains("nested/.git/config"),
        "current/nested/.git/config must survive sync: {:?}",
        after_current
    );
    for (rel, expected) in [
        (".git/config", "seed-parent-git"),
        ("parent-keep.txt", "seed-parent-keep"),
    ] {
        assert_eq!(
            read_remote_text(&client, &build_file_url(&test_url, rel)),
            expected,
            "parent {} must be untouched by a scoped sync",
            rel
        );
    }
    assert_eq!(
        fs::read_to_string(download_parent.join(".git/config")).unwrap(),
        "download-parent-git",
        "local parent .git must be untouched by a scoped sync"
    );

    remove_remote_tree(&client, &test_url, &cancel_flag);
    let _ = fs::remove_dir_all(&temp_root);
}

#[test]
fn test_scan_prunes_excluded_folders_without_requesting_them() {
    use std::collections::HashMap;
    use std::io::{Read, Write};
    use std::net::{TcpListener, TcpStream};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex};

    // The mock server runs on loopback, which SSRF validation blocks unless allowed.
    std::env::set_var("ALLOW_INSECURE_WEBDAV", "1");

    // Remote tree:
    //   keep.txt
    //   .git/config                        <- excluded by name at the scan root
    //   .gitlab/notes.txt                  <- near miss: must stay included
    //   assets/keep.txt                    <- included
    //   assets/cache/data.bin              <- excluded by path pattern
    //   assets/cache2/keep.txt             <- near miss: must stay included
    //   build/keep.txt                     <- included: only build/cache is excluded
    //   build/cache/artifact.bin           <- excluded by relative path
    //   module/notes.txt                   <- included
    //   module/.git/config                 <- excluded by name, one level deep
    //   module/deep/keep.txt               <- included
    //   module/deep/.git/config            <- excluded by name, two levels deep
    //   node_modules/pkg/index.js          <- excluded by name
    //   src/main.rs                        <- included
    type Entry = (Vec<&'static str>, Vec<(&'static str, u64)>);
    let mut dirs: HashMap<&'static str, Entry> = HashMap::new();
    dirs.insert(
        "/exclude-course/",
        (
            vec![
                ".git/",
                ".gitlab/",
                "assets/",
                "build/",
                "module/",
                "node_modules/",
                "src/",
            ],
            vec![("keep.txt", 10)],
        ),
    );
    dirs.insert("/exclude-course/.git/", (vec![], vec![("config", 20)]));
    dirs.insert(
        "/exclude-course/.gitlab/",
        (vec![], vec![("notes.txt", 15)]),
    );
    dirs.insert(
        "/exclude-course/assets/",
        (vec!["cache/", "cache2/"], vec![("keep.txt", 35)]),
    );
    dirs.insert(
        "/exclude-course/assets/cache/",
        (vec![], vec![("data.bin", 45)]),
    );
    dirs.insert(
        "/exclude-course/assets/cache2/",
        (vec![], vec![("keep.txt", 55)]),
    );
    dirs.insert(
        "/exclude-course/build/",
        (vec!["cache/"], vec![("keep.txt", 30)]),
    );
    dirs.insert(
        "/exclude-course/build/cache/",
        (vec![], vec![("artifact.bin", 40)]),
    );
    dirs.insert(
        "/exclude-course/module/",
        (vec![".git/", "deep/"], vec![("notes.txt", 60)]),
    );
    dirs.insert(
        "/exclude-course/module/.git/",
        (vec![], vec![("config", 65)]),
    );
    dirs.insert(
        "/exclude-course/module/deep/",
        (vec![".git/"], vec![("keep.txt", 70)]),
    );
    dirs.insert(
        "/exclude-course/module/deep/.git/",
        (vec![], vec![("config", 75)]),
    );
    dirs.insert(
        "/exclude-course/node_modules/",
        (vec!["pkg/"], vec![]),
    );
    dirs.insert(
        "/exclude-course/node_modules/pkg/",
        (vec![], vec![("index.js", 80)]),
    );
    dirs.insert("/exclude-course/src/", (vec![], vec![("main.rs", 50)]));

    let mut responses: HashMap<String, Vec<u8>> = HashMap::new();
    for (dir_path, (subfolders, files)) in &dirs {
        let mut xml = String::new();
        xml.push_str(r#"<?xml version="1.0" encoding="utf-8"?><D:multistatus xmlns:D="DAV:">"#);
        xml.push_str(&format!(
            r#"<D:response><D:href>{}</D:href><D:propstat><D:prop><D:resourcetype><D:collection/></D:resourcetype></D:prop><D:status>HTTP/1.1 200 OK</D:status></D:propstat></D:response>"#,
            dir_path
        ));
        for sub in subfolders {
            let full_sub = format!("{}{}", dir_path, sub);
            xml.push_str(&format!(
                r#"<D:response><D:href>{}</D:href><D:propstat><D:prop><D:resourcetype><D:collection/></D:resourcetype></D:prop><D:status>HTTP/1.1 200 OK</D:status></D:propstat></D:response>"#,
                full_sub
            ));
        }
        for (file, size) in files {
            let full_file = format!("{}{}", dir_path, file);
            xml.push_str(&format!(
                r#"<D:response><D:href>{}</D:href><D:propstat><D:prop><D:resourcetype/><D:getcontentlength>{}</D:getcontentlength><D:getlastmodified>Mon, 28 Sep 2026 12:00:00 GMT</D:getlastmodified></D:prop><D:status>HTTP/1.1 200 OK</D:status></D:propstat></D:response>"#,
                full_file, size
            ));
        }
        xml.push_str("</D:multistatus>");

        let response = format!(
            "HTTP/1.1 207 Multi-Status\r\nContent-Type: application/xml; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            xml.len(),
            xml
        );
        responses.insert(
            dir_path.trim_end_matches('/').to_ascii_lowercase(),
            response.into_bytes(),
        );
    }

    let responses = Arc::new(responses);
    let requested: Arc<Mutex<HashSet<String>>> = Arc::new(Mutex::new(HashSet::new()));

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let is_running = Arc::new(AtomicBool::new(true));

    let server_running = is_running.clone();
    let server_responses = responses.clone();
    let server_requested = requested.clone();
    let server_handle = std::thread::spawn(move || {
        while server_running.load(Ordering::SeqCst) {
            let (mut stream, _) = match listener.accept() {
                Ok(conn) => conn,
                Err(_) => break,
            };
            if !server_running.load(Ordering::SeqCst) {
                break;
            }
            let resp_map = server_responses.clone();
            let requested_map = server_requested.clone();
            std::thread::spawn(move || {
                let mut buf = [0u8; 4096];
                let n = match stream.read(&mut buf) {
                    Ok(n) if n > 0 => n,
                    _ => return,
                };
                let request = String::from_utf8_lossy(&buf[..n]);

                // Match the D2L/IIS behavior the app falls back from: Depth: infinity is refused.
                if request.to_ascii_lowercase().contains("depth: infinity") {
                    let forbidden =
                        "HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
                    let _ = stream.write_all(forbidden.as_bytes());
                    return;
                }

                let first_line = request.lines().next().unwrap_or("");
                let path = first_line.split_whitespace().nth(1).unwrap_or("/");
                let lookup_key = path.trim_end_matches('/').to_ascii_lowercase();
                if let Ok(mut guard) = requested_map.lock() {
                    guard.insert(lookup_key.clone());
                }

                if let Some(response) = resp_map.get(&lookup_key) {
                    let _ = stream.write_all(response);
                } else {
                    let not_found =
                        "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
                    let _ = stream.write_all(not_found.as_bytes());
                }
            });
        }
    });

    let base_url = format!("http://127.0.0.1:{}/exclude-course/", port);
    let client = Client::init("testuser", "testpass", &base_url).expect("client init");
    let cancel_flag = AtomicBool::new(false);
    let exclusions = normalize_exclude_folders(&[
        ".git/".to_string(),
        "node_modules".to_string(),
        "assets/cache".to_string(),
        "build/cache".to_string(),
    ]);
    let mut log_output = String::new();

    let items = list_remote_recursive_with_exclusions_and_log(
        &client,
        &base_url,
        &cancel_flag,
        Some(4),
        None,
        true,
        &exclusions,
        None,
        |msg| log_output.push_str(msg),
    )
    .expect("scan with exclusions failed");

    let mut found: Vec<String> = items
        .iter()
        .filter(|item| !item.is_dir)
        .map(|item| relative_item_path(&base_url, &item.href))
        .collect();
    found.sort();
    assert_eq!(
        found,
        vec![
            ".gitlab/notes.txt".to_string(),
            "assets/cache2/keep.txt".to_string(),
            "assets/keep.txt".to_string(),
            "build/keep.txt".to_string(),
            "keep.txt".to_string(),
            "module/deep/keep.txt".to_string(),
            "module/notes.txt".to_string(),
            "src/main.rs".to_string(),
        ],
        "excluded folders and their contents must not appear in the listing"
    );

    // The strongest guarantee: excluded collections were never requested at all, including
    // `.git` nested one and two levels deep.
    let requested = requested.lock().unwrap().clone();
    for skipped in [
        "/exclude-course/.git",
        "/exclude-course/module/.git",
        "/exclude-course/module/deep/.git",
        "/exclude-course/node_modules",
        "/exclude-course/assets/cache",
        "/exclude-course/build/cache",
    ] {
        assert!(
            !requested.contains(skipped),
            "excluded folder {} must not be requested; requested: {:?}",
            skipped,
            requested
        );
    }
    for scanned in [
        "/exclude-course",
        "/exclude-course/.gitlab",
        "/exclude-course/assets",
        "/exclude-course/assets/cache2",
        "/exclude-course/build",
        "/exclude-course/module",
        "/exclude-course/module/deep",
        "/exclude-course/src",
    ] {
        assert!(
            requested.contains(scanned),
            "included folder {} must be scanned; requested: {:?}",
            scanned,
            requested
        );
    }
    assert_not_scanned_directories(
        &log_output,
        &[
            ".git",
            "module/.git",
            "module/deep/.git",
            "node_modules",
            "assets/cache",
            "build/cache",
        ],
    );
    assert_scanned_directories(
        &log_output,
        &[
            ".gitlab",
            "assets",
            "assets/cache2",
            "build",
            "module",
            "module/deep",
            "src",
        ],
    );

    is_running.store(false, Ordering::SeqCst);
    let _ = TcpStream::connect(format!("127.0.0.1:{}", port));
    let _ = server_handle.join();
}

/// End-to-end coverage for the extension whitelist filter.
///
/// A project tree with mixed file types is uploaded and downloaded with a
/// whitelist of `.HTML` (dot + casing on purpose) and `txt`; only listed
/// extensions may transfer in either direction. Files without an extension and
/// files whose full extension is not listed (`.tar.gz` vs `gz`) must stay out.
#[test]
fn test_live_copyparty_extension_whitelist_only_listed_extensions_transfer() {
    let base_url = match live_base_url() {
        Some(url) => url,
        None => {
            eprintln!(
                "SKIPPED: TEST_WEBDAV_URL not set; skipping live extension whitelist test."
            );
            return;
        }
    };
    let client = live_client();
    let cancel_flag = AtomicBool::new(false);
    let mut log_output = String::new();

    // Dedicated collection so the test never disturbs the other live tests.
    let test_url = format!("{}/extension_whitelist_test/", base_url.trim_end_matches('/'));
    let _ = client.delete(&test_url);
    let _ = client.mkcol(&test_url);

    let temp_root = std::env::temp_dir().join(format!(
        "scs_extwl_e2e_{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let project_dir = temp_root.join("project");
    let download_dir = temp_root.join("download");

    let write = |root: &std::path::Path, rel: &str, contents: &[u8]| {
        let path = root.join(rel);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, contents).unwrap();
    };

    write(&project_dir, "index.html", b"<html>index</html>");
    write(&project_dir, "docs/notes.TXT", b"notes");
    write(&project_dir, "data.bin", b"binary-data");
    write(&project_dir, "LICENSE", b"license");
    write(&project_dir, "docs/archive.tar.gz", b"gz-bytes");

    let filter = ExtensionFilter::new(
        ExtensionFilterMode::Whitelist,
        &[".HTML".to_string(), "txt".to_string()],
    );

    // 1. Upload: only .html/.txt files are copied, whatever their casing.
    log_output.clear();
    let put_res = execute_webdav_action_with_exclusions(
        &client,
        "put",
        &test_url,
        &project_dir,
        &cancel_flag,
        None,
        None,
        &[],
        Some(&filter),
        |msg| log_output.push_str(msg),
    );
    assert!(
        put_res.is_ok(),
        "PUT with extension whitelist failed: {:?}",
        put_res
    );
    assert!(
        log_output.contains("Copied: index.html"),
        "index.html must upload: {}",
        log_output
    );
    assert!(
        log_output.contains("Copied: docs/notes.TXT"),
        "docs/notes.TXT must upload (case-insensitive extension match): {}",
        log_output
    );
    assert!(
        !log_output.contains("Copied: data.bin"),
        "data.bin must not upload: {}",
        log_output
    );
    assert!(
        !log_output.contains("Copied: LICENSE"),
        "extensionless LICENSE must not upload: {}",
        log_output
    );
    assert!(
        !log_output.contains("Copied: docs/archive.tar.gz"),
        "archive.tar.gz must not upload (full extension not whitelisted): {}",
        log_output
    );

    let remote = remote_file_paths(&client, &test_url, &cancel_flag);
    assert!(remote.contains("index.html"), "remote must hold index.html: {:?}", remote);
    assert!(
        remote.contains("docs/notes.TXT"),
        "remote must hold docs/notes.TXT: {:?}",
        remote
    );
    assert!(!remote.contains("data.bin"), "remote must not hold data.bin: {:?}", remote);
    assert!(!remote.contains("LICENSE"), "remote must not hold LICENSE: {:?}", remote);
    assert!(
        !remote.contains("docs/archive.tar.gz"),
        "remote must not hold docs/archive.tar.gz: {:?}",
        remote
    );

    // 2. Download into an empty directory: only whitelisted extensions come down.
    fs::create_dir_all(&download_dir).unwrap();
    log_output.clear();
    let get_res = execute_webdav_action_with_exclusions(
        &client,
        "get",
        &test_url,
        &download_dir,
        &cancel_flag,
        None,
        None,
        &[],
        Some(&filter),
        |msg| log_output.push_str(msg),
    );
    assert!(
        get_res.is_ok(),
        "GET with extension whitelist failed: {:?}",
        get_res
    );
    let mut downloaded: Vec<String> = collect_local_files(&download_dir)
        .into_iter()
        .map(|(_, rel, _)| rel)
        .collect();
    downloaded.sort();
    assert_eq!(
        downloaded,
        vec!["docs/notes.TXT".to_string(), "index.html".to_string()],
        "only whitelisted extensions may download"
    );

    // Cleanup: remove the dedicated remote collection and local temp tree.
    if let Ok(items) = list_remote_recursive(&client, &test_url, &cancel_flag) {
        for item in items {
            let rel = relative_item_path(&test_url, &item.href);
            if !rel.is_empty() {
                let _ = client.delete(&resolve_item_url(&test_url, &item.href));
            }
        }
    }
    let _ = client.delete(&test_url);
    let _ = fs::remove_dir_all(&temp_root);
}

/// End-to-end coverage for the extension blacklist filter.
///
/// With `BIN` blacklisted (casing on purpose), uploads and downloads keep every
/// other file — including files without an extension, which never match.
#[test]
fn test_live_copyparty_extension_blacklist_drops_listed_extensions() {
    let base_url = match live_base_url() {
        Some(url) => url,
        None => {
            eprintln!(
                "SKIPPED: TEST_WEBDAV_URL not set; skipping live extension blacklist test."
            );
            return;
        }
    };
    let client = live_client();
    let cancel_flag = AtomicBool::new(false);
    let mut log_output = String::new();

    let test_url = format!("{}/extension_blacklist_test/", base_url.trim_end_matches('/'));
    let _ = client.delete(&test_url);
    let _ = client.mkcol(&test_url);

    let temp_root = std::env::temp_dir().join(format!(
        "scs_extbl_e2e_{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let project_dir = temp_root.join("project");
    let download_dir = temp_root.join("download");

    let write = |root: &std::path::Path, rel: &str, contents: &[u8]| {
        let path = root.join(rel);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, contents).unwrap();
    };

    write(&project_dir, "page.html", b"<html>page</html>");
    write(&project_dir, "src/main.rs", b"fn main() {}");
    write(&project_dir, "blob.BIN", b"blob-bytes");
    write(&project_dir, "LICENSE", b"license");

    let filter =
        ExtensionFilter::new(ExtensionFilterMode::Blacklist, &["BIN".to_string()]);

    // 1. Upload: everything except the blacklisted extension.
    log_output.clear();
    let put_res = execute_webdav_action_with_exclusions(
        &client,
        "put",
        &test_url,
        &project_dir,
        &cancel_flag,
        None,
        None,
        &[],
        Some(&filter),
        |msg| log_output.push_str(msg),
    );
    assert!(
        put_res.is_ok(),
        "PUT with extension blacklist failed: {:?}",
        put_res
    );
    assert!(
        log_output.contains("Copied: page.html"),
        "page.html must upload: {}",
        log_output
    );
    assert!(
        log_output.contains("Copied: src/main.rs"),
        "src/main.rs must upload: {}",
        log_output
    );
    assert!(
        log_output.contains("Copied: LICENSE"),
        "extensionless LICENSE must upload (never matches a blacklist): {}",
        log_output
    );
    assert!(
        !log_output.contains("Copied: blob.BIN"),
        "blob.BIN must not upload: {}",
        log_output
    );

    let remote = remote_file_paths(&client, &test_url, &cancel_flag);
    assert!(remote.contains("page.html"), "remote must hold page.html: {:?}", remote);
    assert!(
        remote.contains("src/main.rs"),
        "remote must hold src/main.rs: {:?}",
        remote
    );
    assert!(remote.contains("LICENSE"), "remote must hold LICENSE: {:?}", remote);
    assert!(!remote.contains("blob.BIN"), "remote must not hold blob.BIN: {:?}", remote);

    // 2. Download into an empty directory: the blacklisted extension stays remote.
    fs::create_dir_all(&download_dir).unwrap();
    log_output.clear();
    let get_res = execute_webdav_action_with_exclusions(
        &client,
        "get",
        &test_url,
        &download_dir,
        &cancel_flag,
        None,
        None,
        &[],
        Some(&filter),
        |msg| log_output.push_str(msg),
    );
    assert!(
        get_res.is_ok(),
        "GET with extension blacklist failed: {:?}",
        get_res
    );
    let mut downloaded: Vec<String> = collect_local_files(&download_dir)
        .into_iter()
        .map(|(_, rel, _)| rel)
        .collect();
    downloaded.sort();
    assert_eq!(
        downloaded,
        vec![
            "LICENSE".to_string(),
            "page.html".to_string(),
            "src/main.rs".to_string(),
        ],
        "only non-blacklisted extensions may download"
    );

    // Cleanup: remove the dedicated remote collection and local temp tree.
    if let Ok(items) = list_remote_recursive(&client, &test_url, &cancel_flag) {
        for item in items {
            let rel = relative_item_path(&test_url, &item.href);
            if !rel.is_empty() {
                let _ = client.delete(&resolve_item_url(&test_url, &item.href));
            }
        }
    }
    let _ = client.delete(&test_url);
    let _ = fs::remove_dir_all(&temp_root);
}
