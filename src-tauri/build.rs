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
    let require_real = std::env::var("TAURI_BUNDLE").is_ok()
        || std::env::var("PROFILE").as_deref() == Ok("release");

    for target in targets {
        let path = binaries_dir.join(target);
        if !path.exists() {
            if require_real {
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
        } else if path.metadata().map(|m| m.len() == 0).unwrap_or(false) && require_real {
            panic!(
                "placeholder (0-byte) sidecar binary {} present in release/bundle build; refusing to ship",
                path.display()
            );
        }
    }

    tauri_build::build();
}
