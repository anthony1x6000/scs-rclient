use scs_rclient_lib::webdav::{
    build_file_url, collect_local_files, compute_sha256, execute_webdav_action,
    list_remote_recursive, parse_propfind_xml, relative_item_path, resolve_item_url,
    verify_webdav_auth,
};
use std::collections::HashSet;
use std::fs;
use std::sync::atomic::AtomicBool;

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
    assert!(rustydav::prelude::Url::parse(&built).is_ok());
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
    let client = rustydav::client::Client::init(&username, &password);

    // 1. Verify Authentication
    assert!(
        verify_webdav_auth(&base_url, &username, &password).is_ok(),
        "verify_webdav_auth should succeed with valid credentials"
    );
    assert!(
        verify_webdav_auth(&base_url, &username, "wrongpassword").is_err(),
        "verify_webdav_auth should fail with invalid credentials"
    );

    // 2. Prepare local test directories and files
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

    let file1_path = local_sync_dir.join("file1.txt");
    let file1_content = "Hello from WebDAV test file 1!\nTesting copyparty PUT and GET roundtrip.";
    fs::write(&file1_path, file1_content).unwrap();
    let file1_sha256 = compute_sha256(file1_content.as_bytes());

    let file_spaces_path = local_sync_dir.join("file with spaces.txt");
    let file_spaces_content = "Contents of file with spaces in filename.\nTesting URL encoding.";
    fs::write(&file_spaces_path, file_spaces_content).unwrap();
    let file_spaces_sha256 = compute_sha256(file_spaces_content.as_bytes());

    let nested_dir = local_sync_dir.join("nested").join("deep");
    fs::create_dir_all(&nested_dir).unwrap();
    let nested_file_path = nested_dir.join("nested_file.txt");
    let nested_content = "Deeply nested file contents.\nTesting recursive traversal.";
    fs::write(&nested_file_path, nested_content).unwrap();
    let nested_sha256 = compute_sha256(nested_content.as_bytes());

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
        log_output.contains("Copied: file1.txt"),
        "PUT output should log file1.txt: {}",
        log_output
    );
    assert!(
        log_output.contains("Copied: file with spaces.txt"),
        "PUT output should log file with spaces.txt: {}",
        log_output
    );

    // 4. Execute LS (Verify listing finds files recursively)
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
    assert!(log_output.contains("file1.txt"), "LS must include file1.txt");
    assert!(
        log_output.contains("file with spaces.txt"),
        "LS must include file with spaces.txt"
    );
    assert!(
        log_output.contains("nested/deep/nested_file.txt"),
        "LS must include nested/deep/nested_file.txt"
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
    assert!(lsd_res.is_ok());

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
        log_output.contains("Downloaded: file1.txt"),
        "GET must download file1.txt: {}",
        log_output
    );
    assert!(
        log_output.contains("Downloaded: file with spaces.txt"),
        "GET must download file with spaces.txt: {}",
        log_output
    );
    assert!(
        log_output.contains("Downloaded: nested/deep/nested_file.txt"),
        "GET must download nested/deep/nested_file.txt: {}",
        log_output
    );

    // Verify downloaded files and their SHA256 hashes
    let dl_file1 = fs::read_to_string(&file1_path).expect("file1.txt was not downloaded!");
    assert_eq!(dl_file1, file1_content);
    assert_eq!(compute_sha256(dl_file1.as_bytes()), file1_sha256);

    let dl_spaces =
        fs::read_to_string(&file_spaces_path).expect("file with spaces.txt was not downloaded!");
    assert_eq!(dl_spaces, file_spaces_content);
    assert_eq!(compute_sha256(dl_spaces.as_bytes()), file_spaces_sha256);

    let dl_nested =
        fs::read_to_string(&nested_file_path).expect("nested_file.txt was not downloaded!");
    assert_eq!(dl_nested, nested_content);
    assert_eq!(compute_sha256(dl_nested.as_bytes()), nested_sha256);

    // 8. Execute SYNC: Delete file1.txt locally and sync -> file1.txt deleted on remote
    fs::remove_file(&file1_path).unwrap();
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
        log_output.contains("Deleted remote file not in local: file1.txt"),
        "SYNC must delete remote file1.txt: {}",
        log_output
    );

    // Verify file1.txt is no longer returned by remote listing
    let remote_after_sync = list_remote_recursive(&client, &base_url, &cancel_flag).unwrap();
    let remote_paths: HashSet<String> = remote_after_sync
        .iter()
        .filter(|i| !i.is_dir)
        .map(|i| relative_item_path(&base_url, &i.href))
        .collect();
    assert!(
        !remote_paths.contains("file1.txt"),
        "file1.txt should have been deleted from remote"
    );
    assert!(remote_paths.contains("file with spaces.txt"));
    assert!(remote_paths.contains("nested/deep/nested_file.txt"));

    let _ = fs::remove_dir_all(&tmp_test_dir);
}
