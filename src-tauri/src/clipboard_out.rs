//! Windows clipboard export.
//! - v1.3.4: put a list of file paths on the clipboard as CF_HDROP so committed
//!   cards can be pasted into Explorer / mail / any file-aware app (Ctrl+C).
//! - v1.3.8: `set_clipboard_image` puts a single picture as CF_DIB so it can be
//!   pasted into image apps (ペイント等). This is a SEPARATE, explicit command
//!   (RMB「クリップボードに画像をコピー」)— Ctrl+C stays files-only, which keeps
//!   the model predictable and leaves room for 動画/音声/選択範囲コピー later.
//!
//! CF_HDROP data layout: a `DROPFILES` header followed by a double-null
//! terminated list of wide (UTF-16) file paths; `pFiles` = sizeof(DROPFILES).

use windows::core::BOOL;
use windows::Win32::Foundation::{GlobalFree, HANDLE, HGLOBAL, POINT};
use windows::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, IsClipboardFormatAvailable, OpenClipboard, SetClipboardData,
};
use windows::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE};
use windows::Win32::System::Ole::{CF_DIB, CF_HDROP};
use windows::Win32::UI::Shell::DROPFILES;

/// True if the clipboard currently holds a file list (CF_HDROP). Used to give
/// guidance on internal Ctrl+V instead of importing files (v1.3.4).
pub fn clipboard_has_files() -> bool {
    unsafe { IsClipboardFormatAvailable(CF_HDROP.0 as u32).is_ok() }
}

/// Replace the clipboard contents with `paths` as a CF_HDROP file list (Ctrl+C).
pub fn set_clipboard_files(paths: &[String]) -> Result<(), String> {
    if paths.is_empty() {
        return Err("コピー対象がありません".into());
    }
    unsafe {
        let hdrop = alloc_hdrop(paths)?;
        if let Err(e) = OpenClipboard(None) {
            let _ = GlobalFree(Some(hdrop));
            return Err(e.to_string());
        }
        let _ = EmptyClipboard();
        let set = SetClipboardData(CF_HDROP.0 as u32, Some(HANDLE(hdrop.0)));
        let _ = CloseClipboard();
        if let Err(e) = set {
            let _ = GlobalFree(Some(hdrop)); // ownership retained on failure
            return Err(e.to_string());
        }
    }
    Ok(())
}

/// Replace the clipboard contents with one picture as CF_DIB (24bpp BGR,
/// bottom-up; alpha composited over white). Lets image apps paste the bitmap
/// directly. Explicit「クリップボードに画像をコピー」command (v1.3.8).
pub fn set_clipboard_image(path: &str) -> Result<(), String> {
    let bytes = build_dib_bytes(path)?;
    unsafe {
        let hdib = alloc_global(&bytes)?;
        if let Err(e) = OpenClipboard(None) {
            let _ = GlobalFree(Some(hdib));
            return Err(e.to_string());
        }
        let _ = EmptyClipboard();
        let set = SetClipboardData(CF_DIB.0 as u32, Some(HANDLE(hdib.0)));
        let _ = CloseClipboard();
        if let Err(e) = set {
            let _ = GlobalFree(Some(hdib));
            return Err(e.to_string());
        }
    }
    Ok(())
}

/// Allocate a movable global holding the CF_HDROP payload (DROPFILES + paths).
unsafe fn alloc_hdrop(paths: &[String]) -> Result<HGLOBAL, String> {
    let mut list: Vec<u16> = Vec::new();
    for p in paths {
        list.extend(p.encode_utf16());
        list.push(0);
    }
    list.push(0); // final terminator after the last path

    let header = std::mem::size_of::<DROPFILES>();
    let total = header + list.len() * std::mem::size_of::<u16>();

    let hglobal: HGLOBAL = GlobalAlloc(GMEM_MOVEABLE, total).map_err(|e| e.to_string())?;
    let base = GlobalLock(hglobal);
    if base.is_null() {
        let _ = GlobalFree(Some(hglobal));
        return Err("メモリのロックに失敗しました".into());
    }
    let df = base as *mut DROPFILES;
    (*df).pFiles = header as u32;
    (*df).pt = POINT { x: 0, y: 0 };
    (*df).fNC = BOOL(0);
    (*df).fWide = BOOL(1);
    let dst = (base as *mut u8).add(header) as *mut u16;
    std::ptr::copy_nonoverlapping(list.as_ptr(), dst, list.len());
    let _ = GlobalUnlock(hglobal);
    Ok(hglobal)
}

/// Allocate a movable global and copy `bytes` into it.
unsafe fn alloc_global(bytes: &[u8]) -> Result<HGLOBAL, String> {
    let hglobal = GlobalAlloc(GMEM_MOVEABLE, bytes.len()).map_err(|e| e.to_string())?;
    let base = GlobalLock(hglobal);
    if base.is_null() {
        let _ = GlobalFree(Some(hglobal));
        return Err("メモリのロックに失敗しました".into());
    }
    std::ptr::copy_nonoverlapping(bytes.as_ptr(), base as *mut u8, bytes.len());
    let _ = GlobalUnlock(hglobal);
    Ok(hglobal)
}

/// Build a CF_DIB payload (BITMAPINFOHEADER + 24bpp BGR, bottom-up) from an
/// image file. Alpha is composited over white. 24-bit BI_RGB is the most
/// universally pasteable form (Paint, classic editors).
fn build_dib_bytes(path: &str) -> Result<Vec<u8>, String> {
    let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
    let img = image::load_from_memory(&bytes)
        .map_err(|e| e.to_string())?
        .to_rgba8();
    let (w, h) = (img.width(), img.height());
    if w == 0 || h == 0 {
        return Err("空の画像です".into());
    }
    let stride = ((w as usize * 3 + 3) / 4) * 4; // rows padded to 4 bytes
    let mut buf = Vec::with_capacity(40 + stride * h as usize);

    // BITMAPINFOHEADER (40 bytes), little-endian.
    buf.extend_from_slice(&40u32.to_le_bytes()); // biSize
    buf.extend_from_slice(&(w as i32).to_le_bytes()); // biWidth
    buf.extend_from_slice(&(h as i32).to_le_bytes()); // biHeight (+ = bottom-up)
    buf.extend_from_slice(&1u16.to_le_bytes()); // biPlanes
    buf.extend_from_slice(&24u16.to_le_bytes()); // biBitCount
    buf.extend_from_slice(&0u32.to_le_bytes()); // biCompression = BI_RGB
    buf.extend_from_slice(&((stride * h as usize) as u32).to_le_bytes()); // biSizeImage
    buf.extend_from_slice(&0i32.to_le_bytes()); // biXPelsPerMeter
    buf.extend_from_slice(&0i32.to_le_bytes()); // biYPelsPerMeter
    buf.extend_from_slice(&0u32.to_le_bytes()); // biClrUsed
    buf.extend_from_slice(&0u32.to_le_bytes()); // biClrImportant

    // Pixels: bottom-up rows, BGR order, alpha composited over white.
    let mut row = vec![0u8; stride];
    for y in (0..h).rev() {
        for b in row.iter_mut() {
            *b = 0;
        }
        for x in 0..w {
            let px = img.get_pixel(x, y).0; // [r, g, b, a]
            let a = px[3] as u32;
            let comp = |c: u8| ((c as u32 * a + 255 * (255 - a)) / 255) as u8;
            let o = x as usize * 3;
            row[o] = comp(px[2]); // B
            row[o + 1] = comp(px[1]); // G
            row[o + 2] = comp(px[0]); // R
        }
        buf.extend_from_slice(&row);
    }
    Ok(buf)
}
