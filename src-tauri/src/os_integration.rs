#![allow(unused_imports)]
use crate::*;
use base64::Engine;
use rusqlite::{Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use tauri::{Emitter, Manager, State};

// ---------------------------------------------------------------------------
// OS integration (no project required)
// ---------------------------------------------------------------------------

pub(crate) fn os_open(path: &str) -> Result<(), String> {
    #[cfg(target_os = "windows")]
    {
        std::process::Command::new("rundll32.exe")
            .args(["url.dll,FileProtocolHandler", path])
            .spawn()
            .map_err(|e| e.to_string())?;
    }
    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("open")
            .arg(path)
            .spawn()
            .map_err(|e| e.to_string())?;
    }
    #[cfg(target_os = "linux")]
    {
        std::process::Command::new("xdg-open")
            .arg(path)
            .spawn()
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}

#[tauri::command]
pub(crate) fn open_with_default(path: String) -> Result<(), String> {
    os_open(&path)
}

/// Show the OS "Open with..." application picker for a file.
#[tauri::command]
pub(crate) fn open_with_chooser(path: String) -> Result<(), String> {
    #[cfg(target_os = "windows")]
    {
        // OpenWith.exe is the modern, reliable "Open with" picker (works for all
        // registered types and Unicode paths). OpenAs_RunDLL is a flaky legacy hack.
        std::process::Command::new("OpenWith.exe")
            .arg(&path)
            .spawn()
            .map_err(|e| e.to_string())?;
    }
    #[cfg(not(target_os = "windows"))]
    {
        // No standard "open with" picker; fall back to the default app.
        os_open(&path)?;
    }
    Ok(())
}

/// Show the "Open with" picker for a URL via a tiny redirect HTML, so the user
/// can choose which browser opens it.
#[tauri::command]
pub(crate) fn open_url_with_chooser(url: String) -> Result<(), String> {
    #[cfg(target_os = "windows")]
    {
        let dir = temp_view_dir();
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        let safe = url.replace('"', "%22");
        let html = format!(
            "<!doctype html><meta charset=\"utf-8\">\
             <meta http-equiv=\"refresh\" content=\"0;url={0}\">\
             <body style=\"font-family:sans-serif\">Opening <a href=\"{0}\">{0}</a> …</body>",
            safe
        );
        let file = dir.join("open_link.html");
        std::fs::write(&file, html).map_err(|e| e.to_string())?;
        std::process::Command::new("OpenWith.exe")
            .arg(&file)
            .spawn()
            .map_err(|e| e.to_string())?;
    }
    #[cfg(not(target_os = "windows"))]
    {
        os_open(&url)?;
    }
    Ok(())
}

#[tauri::command]
pub(crate) fn reveal_in_dir(path: String) -> Result<(), String> {
    #[cfg(target_os = "windows")]
    {
        std::process::Command::new("explorer")
            .args(["/select,", &path])
            .spawn()
            .map_err(|e| e.to_string())?;
    }
    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("open")
            .args(["-R", &path])
            .spawn()
            .map_err(|e| e.to_string())?;
    }
    #[cfg(target_os = "linux")]
    {
        let dir = std::path::Path::new(&path)
            .parent()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or(path.clone());
        std::process::Command::new("xdg-open")
            .arg(&dir)
            .spawn()
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// Swap in our own OLE drop target (files + URL/text). Called once from the
/// frontend after mount, when the WebView2 child windows are guaranteed to
/// exist. No-op on non-Windows, where the built-in drag-drop events remain.
#[tauri::command]
pub(crate) fn install_drop_target(window: tauri::WebviewWindow) -> Result<(), String> {
    #[cfg(windows)]
    {
        let hwnd = window.hwnd().map_err(|e| e.to_string())?.0 as isize;
        let app = window.app_handle().clone();
        window
            .run_on_main_thread(move || drag_drop::install(hwnd, app))
            .map_err(|e| e.to_string())?;
    }
    #[cfg(not(windows))]
    {
        let _ = window;
    }
    Ok(())
}

pub(crate) fn temp_view_dir() -> PathBuf {
    std::env::temp_dir().join("texos_view")
}

