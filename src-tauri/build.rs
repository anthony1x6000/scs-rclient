use std::fs::File;
use std::path::Path;

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
        if !path.exists() {
            if require_real && is_for_this_target {
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
        } else if path.metadata().map(|m| m.len() == 0).unwrap_or(false) && require_real && is_for_this_target {
            panic!(
                "placeholder (0-byte) sidecar binary {} present in release/bundle build; refusing to ship",
                path.display()
            );
        }
    }

    tauri_build::build();
}
