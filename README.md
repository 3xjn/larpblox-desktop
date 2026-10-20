# Larpblox Desktop

A Windows setup and tray app for the local Larpblox browser/game connection. Rust handles the loopback HTTP/WebSocket bridge; React and coss UI provide the setup window. Connections pair automatically. The app stays in the tray until you choose Exit.

The extension lives in [larpblox-extension](https://github.com/3xjn/larpblox-extension). Shared appearance hosting is separate; the local bridge binds only to `127.0.0.1:38261` and does not send data to external servers.

Development: install Rust, Windows C++ build tools, and Bun, then run `bun install` and `bun run desktop`. `bun run package` builds the Windows installer. Lockfiles pin release dependencies.

Use the cog to choose whether Larpblox starts with Windows. Closing the window keeps the connection running; choose Exit from the tray to stop it.

Checks: `bun run build`, `cargo test --manifest-path src-tauri/Cargo.toml`, and `node scripts/check-bridge.mjs` with the app running. The live check uses test account IDs 7 and 8, without changing the signed-in account.
