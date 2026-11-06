# opusic-sys builds its bundled Opus from source with CMake. Keep its CRT
# consistent with Rust's -C target-feature=+crt-static on Windows.
set(OPUS_STATIC_RUNTIME ON CACHE BOOL "Use the static MSVC runtime for Opus" FORCE)
