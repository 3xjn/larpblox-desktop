# Larpblox Desktop

A Windows setup and tray app for the local Larpblox browser/game connection. Rust handles the loopback HTTP/WebSocket bridge; React and coss UI provide the setup window. Connections pair automatically. The app stays in the tray until you choose Exit.

The extension lives in [larpblox-extension](https://github.com/3xjn/larpblox-extension). Shared appearance hosting is separate; the local bridge binds only to `127.0.0.1:38261` and does not send data to external servers.

Development: install Rust, Windows C++ build tools, and Bun, then run `bun install` and `bun run desktop`. `bun run package` builds the Windows installer. Lockfiles pin release dependencies.

Use the cog to choose whether Larpblox starts with Windows. Closing the window keeps the connection running; choose Exit from the tray to stop it.

If another app is using the local connection, close that app and click **Retry connection**. Larpblox retries without losing its saved pairing or restarting the desktop window.

## Windows releases

The **Windows installer release** GitHub Actions workflow builds and uploads only the Windows x64 setup `.exe`. It runs when a `v*` tag is pushed, or manually from the Actions tab with an existing tag such as `v0.1.0`. Ordinary branch pushes do not start a Windows build.

For a new release, update the versions in `package.json`, `src-tauri/tauri.conf.json`, and `src-tauri/Cargo.toml` (and regenerate `src-tauri/Cargo.lock`), commit them, then push the matching version tag. For example, version `0.1.1` uses tag `v0.1.1`. The workflow validates these versions, installs locked dependencies, runs frontend and Rust checks, builds the NSIS installer, and creates or updates the tag's GitHub Release. Tags containing a prerelease suffix produce prereleases. A manual rerun replaces the installer asset for that tag.

Private-repository builds use the owner's GitHub Actions allowance. The workflow does not change billing settings or add signing credentials. Browser extension downloads remain in the separate extension repository.

Checks: `bun run build`, `cargo test --manifest-path src-tauri/Cargo.toml`, and `node scripts/check-bridge.mjs` with the app running. The live check uses test account IDs 7 and 8, without changing the signed-in account.
