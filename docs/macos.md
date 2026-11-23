# TAK Viewer on macOS

Build on a Mac with Rust and Apple Command Line Tools. Run from the source folder.

```bash
bash scripts/01_build_macos.sh
open 'build/TAK Viewer.app'
```

Install or update the Documents copy with the same script.

```bash
bash scripts/01_build_macos.sh "$HOME/Documents/TAK Viewer.app"
```

- Builds for this Mac's processor. Apple Silicon → arm64. The script signs the app for local use and validates the bundle before replacement. A recognized older bundle moves to a hidden backup folder next to the new copy. Keep the build log for its path.
- Open `TAK Viewer.app` in Finder. Use `File → Open File` or drop a `.tak` or `.takm` into the window. Use a `.takm` to save comments and marker edits.
- Command+Z undoes. Command+Y redoes. Command+Shift+S saves captures. `Open Folder` shows them in Finder. Outside a recognized repo, default captures go under `tak_viewer/captures` in the Mac temporary folder.
- 3D orbit → Option+drag or right-button drag. Pan → Option+Shift+drag or middle-button drag. Zoom → scroll or pinch. Graph pan → Option+drag or middle-button drag.
- CLI → `build/TAK Viewer.app/Contents/MacOS/tak_viewer`. Use `--version` or `--help`.
