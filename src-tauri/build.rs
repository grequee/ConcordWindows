fn main() {
    let windows = matches!(
        std::env::var("CARGO_CFG_TARGET_OS").as_deref(),
        Ok("windows")
    );

    if matches!(std::env::var("CARGO_CFG_TARGET_OS").as_deref(), Ok("linux")) {
        println!("cargo:rustc-link-arg=-Wl,-rpath,$ORIGIN/../lib/concord");
        println!("cargo:rustc-link-arg=-Wl,-rpath,$ORIGIN/../lib");
        // libyuv is built with JPEG support on some distributions. Keep libjpeg
        // as a direct dependency so the bundled copy is found through RUNPATH
        // instead of relying on the host's loader cache.
        println!("cargo:rustc-link-arg=-Wl,--no-as-needed,-ljpeg,--as-needed");
    }

    let mut attributes = tauri_build::Attributes::new();
    if windows {
        // Tauri embeds the application manifest only in the main binary. Embed
        // it ourselves for every target so the unit and integration test
        // binaries also get Common-Controls v6 instead of failing to start
        // with STATUS_ENTRYPOINT_NOT_FOUND.
        embed_windows_manifest();
        attributes = attributes
            .windows_attributes(tauri_build::WindowsAttributes::new_without_app_manifest());
    }

    if let Err(error) = tauri_build::try_build(attributes) {
        eprintln!("{error:#}");
        std::process::exit(1);
    }
}

fn embed_windows_manifest() {
    let manifest =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("windows-app-manifest.xml");
    println!("cargo:rerun-if-changed={}", manifest.display());
    println!("cargo:rustc-link-arg=/MANIFEST:EMBED");
    println!("cargo:rustc-link-arg=/MANIFESTINPUT:{}", manifest.display());
}
