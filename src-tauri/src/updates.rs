use std::sync::Mutex;
use tauri_plugin_updater::{Update, UpdaterExt};

#[derive(Default)]
pub struct PendingUpdate(Mutex<Option<Update>>);

#[tauri::command]
pub async fn check_for_updates(
    app: tauri::AppHandle,
    pending: tauri::State<'_, PendingUpdate>,
) -> Result<Option<String>, String> {
    // Clear an earlier result so a failed check cannot install a stale selection.
    *pending
        .0
        .lock()
        .map_err(|_| "Couldn't check for updates.")? = None;
    let update = app
        .updater_builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .map_err(|_| "Couldn't start the update check.")?
        .check()
        .await
        .map_err(|_| "Couldn't check for updates. Check your connection and try again.")?;
    let version = update.as_ref().map(|update| update.version.clone());
    *pending.0.lock().map_err(|_| "Couldn't save the update.")? = update;
    Ok(version)
}

#[tauri::command]
pub async fn install_update(
    app: tauri::AppHandle,
    pending: tauri::State<'_, PendingUpdate>,
) -> Result<(), String> {
    let update = pending
        .0
        .lock()
        .map_err(|_| "Couldn't start the update.")?
        .take()
        .ok_or("Check for updates again before installing.")?;
    update
        .download_and_install(|_, _| {}, || {})
        .await
        .map_err(|_| "Couldn't install the update. Check for updates and try again.")?;
    app.restart();
}
