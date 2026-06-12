//! Windows shell icons: resolve the OS file-association icon for an
//! extension (or a concrete file) and return it as a PNG data URL.
//! On non-Windows platforms the command resolves to None (emoji fallback).

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

fn cache() -> &'static Mutex<HashMap<String, Option<String>>> {
    static CACHE: OnceLock<Mutex<HashMap<String, Option<String>>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Shell icon for `ext` ("png", "blend", ...). When `path` points to an
/// existing file its own icon is used instead (e.g. .exe / .lnk carry one).
#[tauri::command]
pub fn get_os_icon(ext: Option<String>, path: Option<String>) -> Result<Option<String>, String> {
    // Per-file icons only matter for types whose icon is embedded in the file.
    let path = path.filter(|p| {
        let lower = p.to_lowercase();
        (lower.ends_with(".exe") || lower.ends_with(".lnk") || lower.ends_with(".ico"))
            && std::path::Path::new(p).exists()
    });
    let key = match (&path, &ext) {
        (Some(p), _) => p.clone(),
        (None, Some(e)) if !e.is_empty() => format!("ext:{}", e.to_lowercase()),
        _ => return Ok(None),
    };
    if let Some(hit) = cache().lock().unwrap().get(&key) {
        return Ok(hit.clone());
    }
    let icon = imp::shell_icon(ext.as_deref(), path.as_deref());
    cache().lock().unwrap().insert(key, icon.clone());
    Ok(icon)
}

#[cfg(windows)]
mod imp {
    use base64::Engine;
    use image::ImageEncoder;
    use windows::core::PCWSTR;
    use windows::Win32::Graphics::Gdi::{
        DeleteObject, GetDC, GetDIBits, ReleaseDC, BITMAPINFO, BITMAPINFOHEADER, BI_RGB,
        DIB_RGB_COLORS, HDC,
    };
    use windows::Win32::Storage::FileSystem::FILE_ATTRIBUTE_NORMAL;
    use windows::Win32::UI::Shell::{
        SHGetFileInfoW, SHFILEINFOW, SHGFI_ICON, SHGFI_LARGEICON, SHGFI_USEFILEATTRIBUTES,
    };
    use windows::Win32::UI::WindowsAndMessaging::{DestroyIcon, GetIconInfo, HICON, ICONINFO};

    pub fn shell_icon(ext: Option<&str>, path: Option<&str>) -> Option<String> {
        let (name, flags) = match path {
            Some(p) => (p.to_string(), SHGFI_ICON | SHGFI_LARGEICON),
            None => (
                format!("f.{}", ext?),
                SHGFI_ICON | SHGFI_LARGEICON | SHGFI_USEFILEATTRIBUTES,
            ),
        };
        let wide: Vec<u16> = name.encode_utf16().chain(std::iter::once(0)).collect();
        unsafe {
            let mut info = SHFILEINFOW::default();
            let res = SHGetFileInfoW(
                PCWSTR(wide.as_ptr()),
                FILE_ATTRIBUTE_NORMAL,
                Some(&mut info),
                std::mem::size_of::<SHFILEINFOW>() as u32,
                flags,
            );
            if res == 0 || info.hIcon.is_invalid() {
                return None;
            }
            let png = hicon_to_png(info.hIcon);
            let _ = DestroyIcon(info.hIcon);
            png
        }
    }

    unsafe fn hicon_to_png(hicon: HICON) -> Option<String> {
        let mut ii = ICONINFO::default();
        GetIconInfo(hicon, &mut ii).ok()?;
        let hdc = GetDC(None);
        let result = color_bitmap_to_png(hdc, &ii);
        ReleaseDC(None, hdc);
        if !ii.hbmColor.is_invalid() {
            let _ = DeleteObject(ii.hbmColor.into());
        }
        if !ii.hbmMask.is_invalid() {
            let _ = DeleteObject(ii.hbmMask.into());
        }
        result
    }

    unsafe fn color_bitmap_to_png(hdc: HDC, ii: &ICONINFO) -> Option<String> {
        // Monochrome icons keep everything in hbmMask; shell file icons
        // always have a color plane, so just fall back to the emoji icon.
        if ii.hbmColor.is_invalid() {
            return None;
        }
        let mut bmi = BITMAPINFO::default();
        bmi.bmiHeader.biSize = std::mem::size_of::<BITMAPINFOHEADER>() as u32;
        if GetDIBits(hdc, ii.hbmColor, 0, 0, None, &mut bmi, DIB_RGB_COLORS) == 0 {
            return None;
        }
        let w = bmi.bmiHeader.biWidth.max(0) as u32;
        let h = bmi.bmiHeader.biHeight.unsigned_abs();
        if w == 0 || h == 0 || w > 512 || h > 512 {
            return None;
        }
        let mut buf = vec![0u8; (w * h * 4) as usize];
        if get_bits_32bpp(hdc, ii.hbmColor, w, h, &mut buf).is_none() {
            return None;
        }
        // Icons without an alpha channel report 0 everywhere; recover
        // opacity from the AND mask (0 = opaque).
        if buf.chunks_exact(4).all(|p| p[3] == 0) {
            let mut mask = vec![0u8; (w * h * 4) as usize];
            if get_bits_32bpp(hdc, ii.hbmMask, w, h, &mut mask).is_some() {
                for (px, m) in buf.chunks_exact_mut(4).zip(mask.chunks_exact(4)) {
                    px[3] = if m[0] == 0 { 255 } else { 0 };
                }
            } else {
                for px in buf.chunks_exact_mut(4) {
                    px[3] = 255;
                }
            }
        }
        // BGRA -> RGBA
        for px in buf.chunks_exact_mut(4) {
            px.swap(0, 2);
        }
        let mut out = Vec::new();
        image::codecs::png::PngEncoder::new(&mut out)
            .write_image(&buf, w, h, image::ExtendedColorType::Rgba8)
            .ok()?;
        let b64 = base64::engine::general_purpose::STANDARD.encode(&out);
        Some(format!("data:image/png;base64,{b64}"))
    }

    unsafe fn get_bits_32bpp(
        hdc: HDC,
        bitmap: windows::Win32::Graphics::Gdi::HBITMAP,
        w: u32,
        h: u32,
        buf: &mut [u8],
    ) -> Option<()> {
        let mut bmi = BITMAPINFO::default();
        bmi.bmiHeader.biSize = std::mem::size_of::<BITMAPINFOHEADER>() as u32;
        bmi.bmiHeader.biWidth = w as i32;
        bmi.bmiHeader.biHeight = -(h as i32); // top-down rows
        bmi.bmiHeader.biPlanes = 1;
        bmi.bmiHeader.biBitCount = 32;
        bmi.bmiHeader.biCompression = BI_RGB.0;
        let lines = GetDIBits(
            hdc,
            bitmap,
            0,
            h,
            Some(buf.as_mut_ptr().cast()),
            &mut bmi,
            DIB_RGB_COLORS,
        );
        (lines as u32 == h).then_some(())
    }
}

#[cfg(not(windows))]
mod imp {
    pub fn shell_icon(_ext: Option<&str>, _path: Option<&str>) -> Option<String> {
        None
    }
}
