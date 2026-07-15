use std::{env, path::PathBuf};

mod build_config;

fn main() {
    let target = env::var("TARGET").expect("TARGET should be set by cargo");
    let target_os = env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();

    let sidecars = [
        "binaries/yt-dlp",
        "binaries/ffmpeg",
        "binaries/ffprobe",
        "binaries/deno",
    ];

    let available_sidecars: Vec<&str> = sidecars
        .into_iter()
        .filter(|base| sidecar_path(base, &target, &target_os).exists())
        .collect();

    let missing_sidecars: Vec<&str> = sidecars
        .into_iter()
        .filter(|base| !sidecar_path(base, &target, &target_os).exists())
        .collect();

    if !missing_sidecars.is_empty() {
        let profile = env::var("PROFILE").unwrap_or_default();
        if profile == "release" {
            panic!(
                "Missing required downloader runtime sidecars for release build: {}",
                missing_sidecars.join(", ")
            );
        } else {
            println!(
                "cargo:warning=Skipping missing local sidecars: {}",
                missing_sidecars.join(", ")
            );
        }
    }

    let inherited_config = env::var("TAURI_CONFIG").ok();
    let merged_config =
        build_config::merge_external_bins(inherited_config.as_deref(), &available_sidecars)
            .unwrap_or_else(|error| panic!("Failed to merge Tauri build configuration: {error}"));

    env::set_var("TAURI_CONFIG", merged_config);

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
