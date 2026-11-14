#!/bin/bash
# Build the native viewer and an independently runnable, locally signed macOS app.
set -euo pipefail

repo_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
if [[ "$(uname -s)" != Darwin ]]; then
    echo 'This script builds a macOS application. Run it on a Mac.' >&2
    exit 1
fi
if ! command -v cargo >/dev/null 2>&1; then
    echo 'Cargo is missing. Install Rust from https://rustup.rs, then reopen Terminal.' >&2
    exit 1
fi
if ! xcrun --find clang >/dev/null 2>&1; then
    echo 'Apple build tools are missing. Run xcode-select --install first.' >&2
    exit 1
fi
cd "$repo_dir"
# Build for this Mac, even when the caller has a cross-compilation target configured.
host_target="$(rustc -vV | sed -n 's/^host: //p')"
case "$host_target" in
    aarch64-apple-darwin|x86_64-apple-darwin) ;;
    *) echo "Not a native macOS Rust toolchain: $host_target" >&2; exit 1 ;;
esac
export MACOSX_DEPLOYMENT_TARGET=11.0
cargo build --release --locked --package tak_viewer --target "$host_target"
target_dir="$(cargo metadata --format-version 1 --no-deps --locked | plutil -extract target_directory raw -o - -)"
binary="$target_dir/$host_target/release/tak_viewer"
version="$("$binary" --version)"
case "$version" in
    ''|*[!0-9.]*) echo "Unexpected application version: $version" >&2; exit 1 ;;
esac

build_dir="$repo_dir/build"
mkdir -p "$build_dir"
stage="$(mktemp -d "$build_dir/.macos-stage.XXXXXX")"
preserve_stage=false
trap 'if [[ "$preserve_stage" == false ]]; then rm -rf "$stage"; fi' EXIT
app="$stage/TAK Viewer.app"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
cp "$binary" "$app/Contents/MacOS/tak_viewer"
cp "$repo_dir/packaging/macos/Info.plist" "$app/Contents/Info.plist"
/usr/libexec/PlistBuddy -c "Set :CFBundleShortVersionString $version" "$app/Contents/Info.plist"
/usr/libexec/PlistBuddy -c "Set :CFBundleVersion $version" "$app/Contents/Info.plist"
plutil -lint "$app/Contents/Info.plist"
codesign --force --sign - --timestamp=none "$app"
codesign --verify --strict --verbose=2 "$app"

# Stage and validate completely before replacing a previous generated bundle.
final="$build_dir/TAK Viewer.app"
previous="$stage/previous.app"
if [[ -e "$final" || -L "$final" ]]; then
    if [[ -L "$final" ]] || [[ "$(/usr/libexec/PlistBuddy -c 'Print :CFBundleIdentifier' "$final/Contents/Info.plist" 2>/dev/null || true)" != io.github.rukkha1024.tak-viewer ]]; then
        echo "Refusing to replace an unrecognized bundle: $final" >&2
        exit 1
    fi
    mv "$final" "$previous"
fi
if ! mv "$app" "$final"; then
    if [[ -d "$previous" ]] && ! mv "$previous" "$final"; then
        preserve_stage=true
        echo "Previous app retained for recovery: $previous" >&2
    fi
    exit 1
fi
printf 'Built: %s\nVersion: %s\nArchitecture: %s\n' "$final" "$version" "$host_target"
printf 'Launch: open "%s"\n' "$final"
