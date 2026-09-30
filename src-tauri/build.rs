use std::fs::File;
use std::path::Path;

/// The smallest size a real rclone binary can plausibly have. The official
/// release binaries are tens of megabytes, so anything below 1 MiB is either the
/// 0-byte placeholder this script writes for dev builds or a truncated download.
/// Rejecting it in a release/bundle build is what stops a placeholder from being
/// shipped (which the app would then report as
/// "No usable rclone binary found (sidecar and system both unavailable)").
const MIN_REAL_SIDECAR_BYTES: u64 = 1024 * 1024;

fn main() {
    // Ensure the src-tauri/binaries directory exists.
    let binaries_dir = Path::new("binaries");
    if !binaries_dir.exists() {
        std::fs::create_dir_all(binaries_dir).expect("failed to create binaries directory");
    }

    // List of sidecar binaries that Tauri expects based on tauri.conf.json configuration
    let targets = vec![
        "rclone-sidecar-x86_64-pc-windows-msvc.exe",
        "rclone-sidecar-x86_64-unknown-linux-gnu",
    ];

    // Release/CI builds must ship the real verified rclone binary; a placeholder
    // would produce an installer with a broken rclone and no diagnostic.
    // NOTE: each CI platform only stages its own sidecar (Linux downloads the
    // linux binary, Windows the .exe), so only the sidecar matching this build's
    // TARGET triple is required — the other keeps the dev placeholder.
    let target = std::env::var("TARGET").unwrap_or_default();
    let require_real = std::env::var("TAURI_BUNDLE").is_ok()
        || std::env::var("PROFILE").as_deref() == Ok("release");

    for target_name in targets {
        let path = binaries_dir.join(target_name);
        let is_for_this_target = (target_name.contains("windows") && target.contains("windows"))
            || (target_name.contains("unknown-linux-gnu") && target.contains("unknown-linux-gnu"))
            // Unknown/other targets (e.g. local `cargo check` without TARGET): don't enforce.
            || target.is_empty();
        let enforce = require_real && is_for_this_target;

        // A stale or truncated file is just as fatal as a missing one, and a
        // same-profile incremental rebuild may reuse target/<profile>/build
        // outputs, so this check must live here rather than only at staging time.
        let existing_size = path.metadata().ok().map(|m| m.len());
        if let (true, Some(size)) = (enforce, existing_size) {
            if size < MIN_REAL_SIDECAR_BYTES {
                panic!(
                    "sidecar binary {} is only {} bytes (< {}); it is a placeholder or a \
                     truncated download and must not be shipped in a release/bundle build",
                    path.display(),
                    size,
                    MIN_REAL_SIDECAR_BYTES
                );
            }
        }

        if !path.exists() {
            if enforce {
                panic!(
                    "missing real sidecar binary {} for release/bundle build; refusing to ship a placeholder",
                    path.display()
                );
            }
            // Write a dummy/placeholder file so the Tauri build/dev step does not fail.
            // In development, the app will execute the system-installed rclone executable.
            // On CI (GitHub Actions), the real verified binary is downloaded and placed here
            // prior to compilation, so it exists and will NOT be overwritten by this dummy file.
            println!(
                "cargo:warning=creating 0-byte placeholder sidecar {}; local builds will use system rclone",
                path.display()
            );
            File::create(&path).expect("failed to create dummy sidecar binary");
        }
    }

    tauri_build::build();
}
