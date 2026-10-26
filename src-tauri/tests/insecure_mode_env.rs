//! Production posture of the env-keyed insecure-mode switch.
//!
//! Integration binary: the library is compiled WITHOUT cfg(test) here, so
//! `validate_webdav_url` behaves exactly as in the shipped app.

use scs_rclient_lib::webdav::validate_webdav_url;

#[test]
fn test_webdav_url_presence_alone_must_not_disable_the_guard() {
    // A launch environment that merely carries the live-test fixture variable
    // (any value, including empty) must not turn off HTTPS/private-range
    // enforcement: only the explicit ALLOW_INSECURE_WEBDAV=1 override may.
    std::env::set_var("TEST_WEBDAV_URL", "");
    std::env::remove_var("ALLOW_INSECURE_WEBDAV");
    assert!(
        validate_webdav_url("http://127.0.0.1:3923/content/").is_err(),
        "TEST_WEBDAV_URL presence must not allow cleartext loopback WebDAV"
    );
    assert!(
        validate_webdav_url("http://169.254.169.254/latest/meta-data/").is_err(),
        "TEST_WEBDAV_URL presence must not allow metadata-range WebDAV"
    );

    // The documented development escape hatch still works.
    std::env::set_var("ALLOW_INSECURE_WEBDAV", "1");
    assert!(
        validate_webdav_url("http://127.0.0.1:3923/content/").is_ok(),
        "ALLOW_INSECURE_WEBDAV=1 must keep enabling local test servers"
    );

    // Restore: do not leak the override into other tests in this binary.
    std::env::remove_var("ALLOW_INSECURE_WEBDAV");
    std::env::remove_var("TEST_WEBDAV_URL");
}
