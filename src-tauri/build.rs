fn main() {
    if matches!(std::env::var("CARGO_CFG_TARGET_OS").as_deref(), Ok("linux")) {
        println!("cargo:rustc-link-arg=-Wl,-rpath,$ORIGIN/../lib/concord");
        println!("cargo:rustc-link-arg=-Wl,-rpath,$ORIGIN/../lib");
        // libyuv is built with JPEG support on some distributions. Keep libjpeg
        // as a direct dependency so the bundled copy is found through RUNPATH
        // instead of relying on the host's loader cache.
        println!("cargo:rustc-link-arg=-Wl,--no-as-needed,-ljpeg,--as-needed");
    }
    tauri_build::build()
}
