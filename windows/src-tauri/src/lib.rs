// ACS for Windows — Tauri shell over the bundled acs-desktop helper.
// Commands mirror what WorkspaceStore.swift needs: one request channel to the
// helper plus local workspace path helpers (project hash, sample dir, reveal).

pub mod helper;

use helper::AcsError;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::path::Path;

/// Send one acs-desktop action. `payload_json` is the exact JSON object text
/// the frontend lossless-stringified — parsed once into a Value (arbitrary
/// precision) and forwarded verbatim, so integer ids never pass through f64.
#[tauri::command]
fn acs_request(db_path: String, action: String, payload_json: String) -> Result<String, AcsError> {
    let payload: Value = serde_json::from_str(&payload_json)
        .map_err(|e| AcsError::MalformedResponse(format!("payload is not JSON: {e}")))?;
    if !payload.is_object() {
        return Err(AcsError::MalformedResponse("payload must be an object".into()));
    }
    helper::request(&helper::helper_path(), Path::new(&db_path), &action, payload)
}

/// `%APPDATA%\ACS` — the app's workspace root (Workspaces/, Samples/). This is
/// the same spot as the macOS app's `~/Library/Application Support/ACS`.
/// Deliberately not `%LOCALAPPDATA%\ACS`: the NSIS per-user installer unpacks
/// the app itself there, and its uninstaller removes that whole directory —
/// bus data must not live inside it.
#[tauri::command]
fn acs_base_dir() -> Result<String, AcsError> {
    let base = dirs::data_dir()
        .ok_or_else(|| AcsError::HelperUnavailable("no APPDATA available".into()))?
        .join("ACS");
    Ok(base.to_string_lossy().into_owned())
}

/// bus.db path for a project folder: Workspaces/<sha256 of canonical path>/.
/// Matches the macOS layout (Application Support/ACS/Workspaces/<hash>/bus.db).
#[tauri::command]
fn acs_workspace_db_path(project_path: String) -> Result<String, AcsError> {
    let canonical = std::fs::canonicalize(&project_path)
        .map_err(|e| AcsError::HelperUnavailable(format!("{project_path}: {e}")))?;
    let key = hex_sha256(canonical.to_string_lossy().as_bytes());
    let base = acs_base_dir()?;
    Ok(Path::new(&base)
        .join("Workspaces")
        .join(key)
        .join("bus.db")
        .to_string_lossy()
        .into_owned())
}

/// A fresh simulated-bus path under Samples/<uuid>/bus.db.
#[tauri::command]
fn acs_sample_db_path() -> Result<String, AcsError> {
    let base = acs_base_dir()?;
    Ok(Path::new(&base)
        .join("Samples")
        .join(uuid::Uuid::new_v4().to_string())
        .join("bus.db")
        .to_string_lossy()
        .into_owned())
}

/// Select the bus database in Explorer.
#[tauri::command]
fn acs_reveal(path: String) -> Result<(), AcsError> {
    std::process::Command::new("explorer.exe")
        .arg(format!("/select,{}", path))
        .spawn()
        .map_err(|e| AcsError::HelperUnavailable(format!("cannot open Explorer: {e}")))?;
    Ok(())
}

fn hex_sha256(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .invoke_handler(tauri::generate_handler![
            acs_request,
            acs_base_dir,
            acs_workspace_db_path,
            acs_sample_db_path,
            acs_reveal,
        ])
        .run(tauri::generate_context!())
        .expect("error while running ACS");
}
