# Sidecar binaries

Tauri's `externalBin` requires the platform-suffixed `duduclaw` binary to live
here before bundling (TODO §D0). The suffix is the Rust **host target triple**:

- macOS (Apple Silicon): `duduclaw-aarch64-apple-darwin`
- macOS (Intel):         `duduclaw-x86_64-apple-darwin`
- Windows:               `duduclaw-x86_64-pc-windows-msvc.exe`
- Linux:                 `duduclaw-x86_64-unknown-linux-gnu`

Stage it from a release build with the helper:

```bash
scripts/desktop/stage-sidecar.sh
```

This directory is git-ignored except for this README (the binaries are build
artifacts, not source).
