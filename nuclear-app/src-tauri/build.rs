use std::{env, path::PathBuf};

fn main() {
    let target = env::var("TARGET").expect("TARGET should be set by cargo");
    let target_os = env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();

    let sidecars = ["binaries/yt-dlp", "binaries/ffmpeg", "binaries/ffprobe"];

    let available_sidecars: Vec<&str> = sidecars
        .into_iter()
        .filter(|base| sidecar_path(base, &target, &target_os).exists())
        .collect();

    let missing_sidecars: Vec<&str> = ["binaries/yt-dlp", "binaries/ffmpeg", "binaries/ffprobe"]
        .into_iter()
        .filter(|base| !sidecar_path(base, &target, &target_os).exists())
        .collect();

    if !missing_sidecars.is_empty() {
        println!(
            "cargo:warning=Skipping missing local sidecars: {}",
            missing_sidecars.join(", ")
        );
    }

    let merged_config = serde_json::json!({
        "bundle": {
            "externalBin": available_sidecars,
        }
    });

    env::set_var("TAURI_CONFIG", merged_config.to_string());

    tauri_build::build()
}

fn sidecar_path(base: &str, target: &str, target_os: &str) -> PathBuf {
    let suffix = if target_os == "windows" {
        format!("-{target}.exe")
    } else {
        format!("-{target}")
    };

    PathBuf::from(format!("{base}{suffix}"))
}
