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
        "rclone-x86_64-pc-windows-msvc.exe",
        "rclone-x86_64-unknown-linux-gnu",
    ];

    for target in targets {
        let path = binaries_dir.join(target);
        if !path.exists() {
            // Write a dummy/placeholder file so the Tauri build/dev step does not fail.
            // In development, the app will execute the system-installed rclone executable.
            // On CI (GitHub Actions), the real verified binary is downloaded and placed here
            // prior to compilation, so it exists and will NOT be overwritten by this dummy file.
            File::create(&path).expect("failed to create dummy sidecar binary");
        }
    }

    tauri_build::build();
}
