#!/usr/bin/env python3
"""Export source-only archives from explicit allowlists; never package local state."""
from pathlib import Path
from zipfile import ZIP_DEFLATED, ZipFile

ROOT = Path(__file__).resolve().parents[1]
EXCLUDED = {"target", "node_modules", "dist", "gen", ".git", ".aws", ".agents", ".codex", "conversa", "__pycache__"}
PRIVATE_NAMES = {"concord.toml", "account-session.json", "chat-identity.json", "desktop.json", "audio-devices.json", "microphone.json"}
PRIVATE_SUFFIXES = {".pem", ".key", ".log", ".tmp"}


def eligible(path: Path) -> bool:
    return (path.is_file() and not path.is_symlink()
            and not any(part in EXCLUDED for part in path.relative_to(ROOT).parts)
            and (not path.name.startswith(".env") or path.name == ".env.example")
            and path.name not in PRIVATE_NAMES
            and path.suffix not in PRIVATE_SUFFIXES)


def export(base: Path, files: list[str], directories: list[str], output: str, prefix: str) -> None:
    candidates = [base / name for name in files]
    for directory in directories:
        candidates.extend((base / directory).rglob("*"))
    sources = sorted({path for path in candidates if eligible(path)})
    destination = ROOT / output
    destination.parent.mkdir(parents=True, exist_ok=True)
    temporary = destination.with_suffix(".tmp")
    with ZipFile(temporary, "w", ZIP_DEFLATED, compresslevel=9) as archive:
        for source in sources:
            archive.write(source, str(Path(prefix) / source.relative_to(base)))
    temporary.replace(destination)
    print(f"{output}: {len(sources)} source files, {destination.stat().st_size} bytes")


if __name__ == "__main__":
    export(ROOT,
           ["Cargo.toml", "Cargo.lock", "package.json", "package-lock.json", "README.md",
            "rust-toolchain.toml", "rustfmt.toml", "vite.config.ts", "svelte.config.js",
            "tsconfig.json", ".gitignore", ".dockerignore", ".github/workflows/windows.yml"],
           ["crates", "src-tauri", "ui", "scripts", "docs", "deploy/windows"],
           "target/windows/Concord-Windows-source.zip", "Concord")
    export(ROOT / "ConcordServer",
           ["Cargo.toml", "Cargo.lock", "README.md", "rust-toolchain.toml", "rustfmt.toml",
            "render.yaml", ".gitignore", ".dockerignore"],
           ["crates", "deploy", "scripts", "docs"],
           "target/neon/ConcordServer-Neon-source.zip", "ConcordServer")
