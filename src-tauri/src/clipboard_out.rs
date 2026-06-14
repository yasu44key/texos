//! Windows clipboard export (v1.3.4 クリップボード持ち出し).
//! Put a list of file paths on the clipboard as CF_HDROP so committed cards can
//! be pasted into Explorer, mail, or any app that accepts files. This is the
//! "出す側" — the receiving side (Ctrl+V) lives in `paste_clipboard` (lib.rs).
//!
//! CF_HDROP data layout: a `DROPFILES` header followed by a double-null
//! terminated list of file paths. We always write wide (UTF-16) paths and set
//! `fWide = TRUE`; `pFiles` is the byte offset from the header start to the
//! path list (= sizeof(DROPFILES)).

use windows::core::BOOL;
use windows::Win32::Foundation::{GlobalFree, HANDLE, HGLOBAL, POINT};
use windows::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, IsClipboardFormatAvailable, OpenClipboard, SetClipboardData,
};
use windows::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE};
use windows::Win32::System::Ole::CF_HDROP;
use windows::Win32::UI::Shell::DROPFILES;

/// True if the clipboard currently holds a file list (CF_HDROP). Used to give
/// guidance on internal Ctrl+V instead of importing files (v1.3.4).
pub fn clipboard_has_files() -> bool {
    unsafe { IsClipboardFormatAvailable(CF_HDROP.0 as u32).is_ok() }
}

/// Replace the clipboard contents with `paths` as a CF_HDROP file list.
pub fn set_clipboard_files(paths: &[String]) -> Result<(), String> {
    if paths.is_empty() {
        return Err("コピー対象がありません".into());
    }

    // Build the double-null-terminated wide-char path list.
    let mut list: Vec<u16> = Vec::new();
    for p in paths {
        list.extend(p.encode_utf16());
        list.push(0);
    }
    list.push(0); // final terminator after the last path

    let header = std::mem::size_of::<DROPFILES>();
    let total = header + list.len() * std::mem::size_of::<u16>();

    unsafe {
        let hglobal: HGLOBAL = GlobalAlloc(GMEM_MOVEABLE, total).map_err(|e| e.to_string())?;
        let base = GlobalLock(hglobal);
        if base.is_null() {
            let _ = GlobalFree(Some(hglobal));
            return Err("メモリのロックに失敗しました".into());
        }

        // DROPFILES header.
        let df = base as *mut DROPFILES;
        (*df).pFiles = header as u32;
        (*df).pt = POINT { x: 0, y: 0 };
        (*df).fNC = BOOL(0);
        (*df).fWide = BOOL(1);

        // Path list immediately after the header.
        let dst = (base as *mut u8).add(header) as *mut u16;
        std::ptr::copy_nonoverlapping(list.as_ptr(), dst, list.len());
        let _ = GlobalUnlock(hglobal);

        OpenClipboard(None).map_err(|e| {
            let _ = GlobalFree(Some(hglobal));
            e.to_string()
        })?;
        let _ = EmptyClipboard();
        // On success the system takes ownership of hglobal; on failure we keep it.
        let set = SetClipboardData(CF_HDROP.0 as u32, Some(HANDLE(hglobal.0)));
        let _ = CloseClipboard();
        if let Err(e) = set {
            let _ = GlobalFree(Some(hglobal));
            return Err(e.to_string());
        }
    }
    Ok(())
}
