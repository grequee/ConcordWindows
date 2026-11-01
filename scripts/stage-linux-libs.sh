#!/usr/bin/env bash
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
binary="$root/target/release/concord-app"
out="$root/target/linux-libs"

if [[ ! -x "$binary" ]]; then
  echo "release binary not found: $binary" >&2
  exit 1
fi

rm -rf "$out"
mkdir -p "$out"

while IFS= read -r source; do
  [[ -n "$source" ]] || continue
  cp -L "$source" "$out/$(basename "$source")"
done < <(ldd "$binary" | awk '$1 ~ /^(libvpx|libyuv|libjpeg)\.so/ { print $3 }')

for expected in libvpx.so.12 libyuv.so libjpeg.so.8; do
  if [[ ! -f "$out/$expected" ]]; then
    echo "missing runtime library: $expected" >&2
    exit 1
  fi
done

echo "staged Linux runtime libraries in $out"
