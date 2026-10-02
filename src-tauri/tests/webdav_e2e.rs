use scs_rclient_lib::webdav::{
    build_file_url, collect_local_files, compute_sha256, execute_webdav_action,
    list_remote_recursive, parse_propfind_xml, parse_webdav_date, relative_item_path,
    resolve_item_url, should_upload_file, verify_webdav_auth,
};
use std::collections::HashSet;
use std::fs;
use std::sync::atomic::AtomicBool;

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

    let client = rustydav::client::Client::init(&user, &pass);
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
