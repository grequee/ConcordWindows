//! Generate libvpx bindings only when the `video` feature is enabled.
//!
//! Using the installed headers keeps the ABI version and struct layouts in
//! sync with the runtime library. The generated code is written to OUT_DIR and
//! included by `src/video.rs`.

use std::env;
use std::path::PathBuf;

fn main() {
    println!("cargo:rerun-if-changed=src/wayland_capture.c");
    println!("cargo:rerun-if-changed=src/vpx_ffi.h");
    println!("cargo:rerun-if-env-changed=VPX_INCLUDE_DIR");
    println!("cargo:rerun-if-env-changed=LIBYUV_LIB_DIR");
    println!("cargo:rerun-if-env-changed=VCPKG_ROOT");
    println!("cargo:rerun-if-env-changed=VCPKGRS_TRIPLET");
    println!("cargo:rustc-check-cfg=cfg(concord_libyuv)");
    println!("cargo:rustc-check-cfg=cfg(concord_speexdsp)");

    if env::var_os("CARGO_FEATURE_AUDIO").is_some() {
        if find_library("speexdsp").is_ok() {
            println!("cargo:rustc-cfg=concord_speexdsp");
        } else {
            println!(
                "cargo:warning=SpeexDSP not found; microphone gain is available, noise suppression is disabled"
            );
        }
    }

    if env::var_os("CARGO_FEATURE_VIDEO").is_none() {
        return;
    }

    configure_libyuv();

    if env::var_os("CARGO_FEATURE_WAYLAND").is_some()
        && env::var("CARGO_CFG_TARGET_OS").ok().as_deref() == Some("linux")
    {
        build_wayland();
    }

    if let Err(error) = generate() {
        eprintln!("could not generate libvpx bindings: {error}");
        std::process::exit(1);
    }
}

fn build_wayland() {
    let mut builder = cc::Build::new();
    builder.file("src/wayland_capture.c").flag("-std=gnu11");
    for package in ["libpipewire-0.3", "gio-unix-2.0"] {
        let library = pkg_config::Config::new()
            .probe(package)
            .unwrap_or_else(|error| {
                eprintln!("Wayland capture requires {package} development files: {error}");
                std::process::exit(1);
            });
        builder.includes(library.include_paths);
    }
    builder.compile("concord_wayland_capture");
}

/// Link libyuv when it is available. libyuv has no pkg-config file on Arch, so
/// probe the usual library directories as a fallback. Without it the Rust
/// scaler remains available, just slower.
fn configure_libyuv() {
    if env::var("CARGO_CFG_TARGET_OS").ok().as_deref() == Some("windows") {
        if find_library("libyuv").is_ok() {
            println!("cargo:rustc-cfg=concord_libyuv");
        } else {
            println!("cargo:warning=libyuv not found; using the slower built-in BGRA scaler");
        }
        return;
    }
    if env::var("CARGO_CFG_TARGET_OS").ok().as_deref() != Some("linux") {
        return;
    }
    if pkg_config::Config::new().probe("libyuv").is_ok() {
        println!("cargo:rustc-cfg=concord_libyuv");
        return;
    }

    let mut candidates = Vec::new();
    if let Some(directory) = env::var_os("LIBYUV_LIB_DIR") {
        candidates.push(PathBuf::from(directory));
    }
    candidates.extend(
        [
            "/usr/lib",
            "/usr/lib64",
            "/usr/local/lib",
            "/usr/lib/x86_64-linux-gnu",
            "/usr/lib/aarch64-linux-gnu",
        ]
        .into_iter()
        .map(PathBuf::from),
    );
    for directory in candidates {
        if directory.join("libyuv.so").exists() || directory.join("libyuv.a").exists() {
            println!("cargo:rustc-link-search=native={}", directory.display());
            println!("cargo:rustc-link-lib=yuv");
            println!("cargo:rustc-cfg=concord_libyuv");
            return;
        }
    }
    println!("cargo:warning=libyuv not found; using the slower built-in BGRA scaler");
}

fn generate() -> Result<(), String> {
    let include_paths = find_library("vpx")
        .map_err(|error| format!("libvpx development files are required: {error}"))?;

    let header = PathBuf::from(
        env::var_os("CARGO_MANIFEST_DIR")
            .ok_or_else(|| "CARGO_MANIFEST_DIR is not set".to_owned())?,
    )
    .join("src/vpx_ffi.h");

    let mut builder = bindgen::Builder::default()
        .header(header.to_string_lossy())
        .allowlist_type("^[vV].*")
        .allowlist_var("^[vV].*")
        .allowlist_function("^[vV].*")
        .rustified_enum("^v.*")
        .layout_tests(false)
        .generate_comments(false)
        .trust_clang_mangling(false);
    for path in &include_paths {
        builder = builder.clang_arg(format!("-I{}", path.display()));
    }
    if let Some(include_dir) = env::var_os("VPX_INCLUDE_DIR") {
        builder = builder.clang_arg(format!("-I{}", PathBuf::from(include_dir).display()));
    }

    let bindings = builder
        .generate()
        .map_err(|error| format!("bindgen failed: {error}"))?;
    let out = PathBuf::from(env::var_os("OUT_DIR").ok_or_else(|| "OUT_DIR is not set".to_owned())?)
        .join("vpx_ffi.rs");
    bindings
        .write_to_file(out)
        .map_err(|error| format!("could not write bindings: {error}"))
}

/// Windows codecs are supplied by vcpkg (static libraries, including their
/// transitive dependencies). Linux continues to use the host's pkg-config.
fn find_library(name: &str) -> Result<Vec<PathBuf>, String> {
    if env::var("CARGO_CFG_TARGET_OS").ok().as_deref() == Some("windows") {
        let port = if name == "vpx" { "libvpx" } else { name };
        vcpkg::Config::new()
            .find_package(port)
            .map(|library| library.include_paths)
            .map_err(|error| error.to_string())
    } else {
        pkg_config::Config::new()
            .probe(name)
            .map(|library| library.include_paths)
            .map_err(|error| error.to_string())
    }
}
