#!/bin/bash
# Build a native, locally signed TAK Viewer bundle.
set -euo pipefail

repo_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
if [[ $(uname -s) != Darwin ]]; then
    echo 'Run this script on a Mac.' >&2
    exit 1
fi
final="${1:-$repo_dir/build/TAK Viewer.app}"
if [[ $# -gt 1 || "$final" != /*/TAK\ Viewer.app ]]; then
    echo 'Pass the absolute path ending in /TAK Viewer.app.' >&2
    exit 2
fi
if ! command -v cargo >/dev/null 2>&1 && [[ -x "$HOME/.cargo/bin/cargo" ]]; then
    export PATH="$HOME/.cargo/bin:$PATH"
fi
command -v cargo >/dev/null || { echo 'Install Rust from https://rustup.rs.' >&2; exit 1; }
xcrun --find clang >/dev/null || { echo 'Install Apple Command Line Tools with xcode-select --install.' >&2; exit 1; }

cd "$repo_dir"
host_target="$(rustc -vV | sed -n 's/^host: //p')"
case "$host_target" in
    aarch64-apple-darwin|x86_64-apple-darwin) ;;
    *) echo "Run a native macOS Rust toolchain. Found $host_target." >&2; exit 1 ;;
esac
export MACOSX_DEPLOYMENT_TARGET=11.0
cargo build --release --locked --package tak_viewer --target "$host_target"
target_dir="$(cargo metadata --format-version 1 --no-deps --locked | plutil -extract target_directory raw -o - -)"
binary="$target_dir/$host_target/release/tak_viewer"
version="$("$binary" --version)"
[[ "$version" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || { echo "Unexpected version $version." >&2; exit 1; }

parent="$(dirname "$final")"
mkdir -p "$parent"
stage="$(mktemp -d "$parent/.tak-viewer-stage.XXXXXX")"
install_lock=''
trap 'rm -rf -- "$stage"; if [[ -n "$install_lock" ]]; then rmdir "$install_lock"; fi' EXIT
app="$stage/TAK Viewer.app"
mkdir -p "$app/Contents/MacOS"
cp "$binary" "$app/Contents/MacOS/tak_viewer"
cp "$repo_dir/packaging/macos/Info.plist" "$app/Contents/Info.plist"
/usr/libexec/PlistBuddy -c "Set :CFBundleShortVersionString $version" "$app/Contents/Info.plist"
/usr/libexec/PlistBuddy -c "Set :CFBundleVersion $version" "$app/Contents/Info.plist"
plutil -lint "$app/Contents/Info.plist"
codesign --force --sign - --timestamp=none "$app"
codesign --verify --strict "$app"

lock="$parent/.tak-viewer-install.lock"
mkdir "$lock" || { echo 'Could not lock the install folder.' >&2; exit 1; }
install_lock="$lock"
backup=''
if [[ -e "$final" || -L "$final" ]]; then
    identifier="$(/usr/libexec/PlistBuddy -c 'Print :CFBundleIdentifier' "$final/Contents/Info.plist" 2>/dev/null || true)"
    if [[ -L "$final" || "$identifier" != io.github.rukkha1024.tak-viewer ]]; then
        echo "Existing app is not a recognized TAK Viewer bundle: $final" >&2
        exit 1
    fi
    backup_dir="$(mktemp -d "$parent/.tak-viewer-previous.XXXXXX")"
    backup="$backup_dir/TAK Viewer.app"
    mv "$final" "$backup"
    printf 'Previous app: %s\n' "$backup"
fi
if ! mv "$app" "$final"; then
    if [[ -n "$backup" && ! -e "$final" ]]; then
        mv "$backup" "$final"
        rmdir "$backup_dir"
    fi
    exit 1
fi
printf 'Built: %s\nVersion: %s\nArchitecture: %s\n' "$final" "$version" "$host_target"
