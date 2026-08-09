//! Windows OLE drag **source** — TexoS のカードを OS 側へドラッグで手渡す（v1.8.1）。
//! `drag_drop.rs`（IDropTarget＝受け側）の対になるモジュール。
//!
//! 実装の要点は「IDataObject を自前で書かない」こと。ドラッグ元を素直に作ると
//! IDataObject の9メソッド＋IEnumFORMATETC まで手実装することになるが、シェルの
//! `SHCreateDataObject` が空のデータオブジェクトを返してくれるので、そこへ
//! CF_HDROP を `SetData` するだけで済む。ドラッグ本体も `SHDoDragDrop` に任せると
//! 既定の IDropSource（Esc で中止・ボタンを離したらドロップ）とドラッグ画像が
//! ついてくる。結果として受け側（214行）より短い。
//!
//! ペイロードは `clipboard_out::alloc_hdrop` をそのまま共用する＝Ctrl+C の
//! クリップボード持ち出しとドラッグアウトは、経路が違うだけで同じバイト列を渡す。
//!
//! HTML5 の dragstart では他アプリに実ファイルを渡せない（ブラウザのサンドボックス内
//! で完結する）ため、ここだけはネイティブの OLE ドラッグを通す必要がある。

use std::os::raw::c_void;

use tauri::Emitter;
use windows::Win32::Foundation::{GlobalFree, HWND};
use windows::Win32::System::Com::{
    IDataObject, DVASPECT_CONTENT, FORMATETC, STGMEDIUM, STGMEDIUM_0, TYMED_HGLOBAL,
};
use windows::Win32::System::Ole::{IDropSource, CF_HDROP, DROPEFFECT_COPY, DROPEFFECT_NONE};
use windows::Win32::UI::Shell::{SHCreateDataObject, SHDoDragDrop};

use crate::clipboard_out::alloc_hdrop;

/// `paths` を CF_HDROP に載せて OLE ドラッグを開始する。
///
/// **メインスレッド（OleInitialize 済み）から呼ぶこと。** `SHDoDragDrop` はマウスを
/// 奪うモーダルループなので、この関数から戻ったときにはドロップ（または中止）は
/// 完了している。ドラッグ中は前面を触れないので、結果は完了後に
/// `texos://drag-out` イベントで返す。
pub fn start_file_drag(hwnd: isize, paths: &[String], app: &tauri::AppHandle) {
    let result = unsafe { do_drag(hwnd, paths) };
    let _ = app.emit(
        "texos://drag-out",
        serde_json::json!({
            "dropped": matches!(result, Ok(true)),
            "error": result.err(),
        }),
    );
}

/// 戻り値 true = どこかに実際にドロップされた（false は Esc や無効な場所で中止）。
unsafe fn do_drag(hwnd: isize, paths: &[String]) -> Result<bool, String> {
    if paths.is_empty() {
        return Err("持ち出せるカードがありません".into());
    }
    let hwnd = HWND(hwnd as *mut c_void);
    let hglobal = unsafe { alloc_hdrop(paths) }?;

    // シェル既製の空データオブジェクト（pidlfolder/apidl/pdtinner すべて未指定）。
    let data: IDataObject = unsafe { SHCreateDataObject(None, None, None::<&IDataObject>) }
        .map_err(|e| {
            unsafe { let _ = GlobalFree(Some(hglobal)); };
            e.to_string()
        })?;

    let fmt = FORMATETC {
        cfFormat: CF_HDROP.0,
        ptd: std::ptr::null_mut(),
        dwAspect: DVASPECT_CONTENT.0,
        lindex: -1,
        tymed: TYMED_HGLOBAL.0 as u32,
    };
    let medium = STGMEDIUM {
        tymed: TYMED_HGLOBAL.0 as u32,
        u: STGMEDIUM_0 { hGlobal: hglobal },
        pUnkForRelease: std::mem::ManuallyDrop::new(None),
    };
    // frelease = true: HGLOBAL の所有権はデータオブジェクトへ移る（解放は向こうの責務）。
    unsafe { data.SetData(&fmt, &medium, true) }.map_err(|e| {
        unsafe { let _ = GlobalFree(Some(hglobal)); };
        e.to_string()
    })?;

    // 効果はコピー限定。移動を許すと相手アプリが元ファイルを消せてしまう
    //（クリップボード持ち出しが Preferred DropEffect を読まないのと同じ判断）。
    let effect = unsafe { SHDoDragDrop(Some(hwnd), &data, None::<&IDropSource>, DROPEFFECT_COPY) }
        .map_err(|e| e.to_string())?;
    Ok(effect.0 != DROPEFFECT_NONE.0)
}
