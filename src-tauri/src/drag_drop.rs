//! Custom OLE drop target for Windows. wry's built-in handler only accepts
//! CF_HDROP (files) and silently swallows everything else, so URL drags from
//! a browser never reach the page. We re-register our own IDropTarget over
//! wry's (same child-window enumeration) and forward both files and text/URL
//! drops to the frontend as "texos://drag" events.

use std::cell::{Cell, RefCell};
use std::os::raw::c_void;

use tauri::Emitter;
use windows::core::{implement, BOOL};
use windows::Win32::Foundation::{DRAGDROP_E_INVALIDHWND, HWND, LPARAM, POINT, POINTL};
use windows::Win32::Graphics::Gdi::ScreenToClient;
use windows::Win32::System::Com::{IDataObject, DVASPECT_CONTENT, FORMATETC, TYMED_HGLOBAL};
use windows::Win32::System::Memory::{GlobalLock, GlobalUnlock};
use windows::Win32::System::Ole::{
    IDropTarget, IDropTarget_Impl, RegisterDragDrop, ReleaseStgMedium, RevokeDragDrop, CF_HDROP,
    CF_UNICODETEXT, DROPEFFECT, DROPEFFECT_COPY, DROPEFFECT_NONE,
};
use windows::Win32::System::SystemServices::MODIFIERKEYS_FLAGS;
use windows::Win32::UI::Shell::{DragFinish, DragQueryFileW, HDROP};
use windows::Win32::UI::WindowsAndMessaging::EnumChildWindows;

// RegisterDragDrop AddRefs the target, but keep our own references alive for
// the lifetime of the process (main thread only) to be safe.
thread_local! {
    static TARGETS: RefCell<Vec<IDropTarget>> = const { RefCell::new(Vec::new()) };
}

/// Replace the drop target of every child window (the WebView2 HWNDs) of
/// `hwnd`. Must run on the main (OLE-initialized) thread, after the webview
/// has been created — invoked from the frontend once React has mounted.
pub fn install(hwnd: isize, app: tauri::AppHandle) {
    let hwnd = HWND(hwnd as *mut c_void);

    let mut callback = |child: HWND| {
        inject(child, &app);
        true
    };
    let mut trait_obj: &mut dyn FnMut(HWND) -> bool = &mut callback;
    let closure_ptr: *mut c_void = (&mut trait_obj) as *mut _ as *mut c_void;

    unsafe extern "system" fn enumerate_callback(hwnd: HWND, lparam: LPARAM) -> BOOL {
        let closure = unsafe { &mut *(lparam.0 as *mut c_void as *mut &mut dyn FnMut(HWND) -> bool) };
        closure(hwnd).into()
    }
    let _ = unsafe { EnumChildWindows(Some(hwnd), Some(enumerate_callback), LPARAM(closure_ptr as _)) };
}

fn inject(hwnd: HWND, app: &tauri::AppHandle) {
    let target: IDropTarget = TexosDropTarget {
        hwnd,
        app: app.clone(),
        effect: Cell::new(DROPEFFECT_NONE),
        has_payload: Cell::new(false),
    }
    .into();
    unsafe {
        if RevokeDragDrop(hwnd) != Err(DRAGDROP_E_INVALIDHWND.into())
            && RegisterDragDrop(hwnd, &target).is_ok()
        {
            TARGETS.with(|t| t.borrow_mut().push(target));
        }
    }
}

#[implement(IDropTarget)]
struct TexosDropTarget {
    hwnd: HWND,
    app: tauri::AppHandle,
    effect: Cell<DROPEFFECT>,
    has_payload: Cell<bool>,
}

fn format(cf: u16) -> FORMATETC {
    FORMATETC {
        cfFormat: cf,
        ptd: std::ptr::null_mut(),
        dwAspect: DVASPECT_CONTENT.0,
        lindex: -1,
        tymed: TYMED_HGLOBAL.0 as u32,
    }
}

impl TexosDropTarget {
    fn client_point(&self, pt: &POINTL) -> POINT {
        let mut pt = POINT { x: pt.x, y: pt.y };
        let _ = unsafe { ScreenToClient(self.hwnd, &mut pt) };
        pt
    }

    fn emit(&self, kind: &str, pt: POINT, paths: Option<Vec<String>>, text: Option<String>) {
        let _ = self.app.emit(
            "texos://drag",
            serde_json::json!({
                "kind": kind,
                "paths": paths,
                "text": text,
                "x": pt.x,
                "y": pt.y,
            }),
        );
    }
}

unsafe fn data_has_payload(data: windows_core::Ref<'_, IDataObject>) -> bool {
    let Some(data) = data.as_ref() else { return false };
    unsafe {
        data.QueryGetData(&format(CF_HDROP.0)).is_ok()
            || data.QueryGetData(&format(CF_UNICODETEXT.0)).is_ok()
    }
}

unsafe fn read_paths(data: &IDataObject) -> Option<(Vec<String>, HDROP)> {
    let medium = unsafe { data.GetData(&format(CF_HDROP.0)) }.ok()?;
    let hdrop = HDROP(medium.u.hGlobal.0 as _);
    let count = unsafe { DragQueryFileW(hdrop, 0xFFFFFFFF, None) };
    let mut paths = Vec::with_capacity(count as usize);
    for i in 0..count {
        let len = unsafe { DragQueryFileW(hdrop, i, None) } as usize;
        let mut buf = vec![0u16; len + 1];
        unsafe { DragQueryFileW(hdrop, i, Some(&mut buf)) };
        paths.push(String::from_utf16_lossy(&buf[..len]));
    }
    Some((paths, hdrop))
}

unsafe fn read_text(data: &IDataObject) -> Option<String> {
    let mut medium = unsafe { data.GetData(&format(CF_UNICODETEXT.0)) }.ok()?;
    let hglobal = unsafe { medium.u.hGlobal };
    let ptr = unsafe { GlobalLock(hglobal) } as *const u16;
    let text = if ptr.is_null() {
        None
    } else {
        let mut len = 0usize;
        while unsafe { *ptr.add(len) } != 0 {
            len += 1;
        }
        Some(String::from_utf16_lossy(unsafe {
            std::slice::from_raw_parts(ptr, len)
        }))
    };
    unsafe {
        let _ = GlobalUnlock(hglobal);
        ReleaseStgMedium(&mut medium);
    }
    text
}

#[allow(non_snake_case)]
impl IDropTarget_Impl for TexosDropTarget_Impl {
    fn DragEnter(
        &self,
        pDataObj: windows_core::Ref<'_, IDataObject>,
        _grfKeyState: MODIFIERKEYS_FLAGS,
        pt: &POINTL,
        pdwEffect: *mut DROPEFFECT,
    ) -> windows::core::Result<()> {
        let valid = unsafe { data_has_payload(pDataObj) };
        self.has_payload.set(valid);
        let effect = if valid { DROPEFFECT_COPY } else { DROPEFFECT_NONE };
        self.effect.set(effect);
        unsafe { *pdwEffect = effect };
        if valid {
            self.emit("enter", self.client_point(pt), None, None);
        }
        Ok(())
    }

    fn DragOver(
        &self,
        _grfKeyState: MODIFIERKEYS_FLAGS,
        pt: &POINTL,
        pdwEffect: *mut DROPEFFECT,
    ) -> windows::core::Result<()> {
        if self.has_payload.get() {
            self.emit("over", self.client_point(pt), None, None);
        }
        unsafe { *pdwEffect = self.effect.get() };
        Ok(())
    }

    fn DragLeave(&self) -> windows::core::Result<()> {
        if self.has_payload.get() {
            self.emit("leave", POINT { x: 0, y: 0 }, None, None);
        }
        self.has_payload.set(false);
        Ok(())
    }

    fn Drop(
        &self,
        pDataObj: windows_core::Ref<'_, IDataObject>,
        _grfKeyState: MODIFIERKEYS_FLAGS,
        pt: &POINTL,
        _pdwEffect: *mut DROPEFFECT,
    ) -> windows::core::Result<()> {
        if !self.has_payload.get() {
            return Ok(());
        }
        self.has_payload.set(false);
        let pt = self.client_point(pt);
        let Some(data) = pDataObj.as_ref() else {
            return Ok(());
        };
        if let Some((paths, hdrop)) = unsafe { read_paths(data) } {
            self.emit("drop", pt, Some(paths), None);
            unsafe { DragFinish(hdrop) };
        } else if let Some(text) = unsafe { read_text(data) } {
            self.emit("drop-text", pt, None, Some(text));
        }
        Ok(())
    }
}
