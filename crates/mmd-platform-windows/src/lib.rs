//! Windows-specific integration for MMD Viewer.
//!
//! The crate shows a native error dialog and the open-file dialog used to
//! choose a model or a motion.

use std::path::PathBuf;

/// Failure while asking Windows to display a dialog.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum PlatformError {
    /// `MessageBoxW` returned zero.
    #[error("Windows failed to display the error dialog")]
    DialogFailed,
}

/// Which file the open dialog should offer.
#[derive(Clone, Copy, Debug)]
pub enum OpenFileKind {
    Model,
    Motion,
    /// Wav, mp3, ogg, or m4a clip played with the motion.
    Audio,
}

/// Identity Windows uses to remember the last folder for this dialog.
///
/// The common item dialog stores one folder per client GUID, so the model
/// dialog and the motion dialog keep separate Explorer history.
pub fn dialog_place(kind: OpenFileKind) -> u128 {
    match kind {
        OpenFileKind::Model => 0x6f0c_2e31_9a14_4c1e_8b7a_2d5e_91c4_a101,
        OpenFileKind::Motion => 0x6f0c_2e31_9a14_4c1e_8b7a_2d5e_91c4_a102,
        OpenFileKind::Audio => 0x6f0c_2e31_9a14_4c1e_8b7a_2d5e_91c4_a103,
    }
}

/// Show a modal error dialog on Windows.
pub fn show_error_dialog(title: &str, message: &str) -> Result<(), PlatformError> {
    #[cfg(windows)]
    {
        show_windows_dialog(title, message)
    }
    #[cfg(not(windows))]
    {
        eprintln!("{title}\n{message}");
        Ok(())
    }
}

/// Open a modal file dialog.
///
/// `Ok(None)` means the user cancelled the dialog.
pub fn pick_open_file(kind: OpenFileKind) -> Result<Option<PathBuf>, PlatformError> {
    #[cfg(windows)]
    {
        pick_windows_file(kind)
    }
    #[cfg(not(windows))]
    {
        let _ = kind;
        Ok(None)
    }
}

/// Encode `value` as a null-terminated UTF-16 string for Win32 APIs.
pub fn wide_null(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(std::iter::once(0)).collect()
}

#[cfg(windows)]
fn show_windows_dialog(title: &str, message: &str) -> Result<(), PlatformError> {
    use windows_sys::Win32::UI::WindowsAndMessaging::{MB_ICONERROR, MB_OK, MessageBoxW};

    let title = wide_null(title);
    let message = wide_null(message);
    // SAFETY: both buffers are null-terminated and live for this call. A null
    // owner window asks Windows for an application-modal dialog with no parent.
    let result = unsafe {
        MessageBoxW(
            std::ptr::null_mut(),
            message.as_ptr(),
            title.as_ptr(),
            MB_OK | MB_ICONERROR,
        )
    };
    if result == 0 {
        Err(PlatformError::DialogFailed)
    } else {
        Ok(())
    }
}

#[cfg(windows)]
fn pick_windows_file(kind: OpenFileKind) -> Result<Option<PathBuf>, PlatformError> {
    use std::os::windows::ffi::OsStringExt;
    use windows_sys::Win32::System::Com::{
        CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx,
        CoTaskMemFree, CoUninitialize,
    };
    use windows_sys::Win32::UI::Shell::{
        FOS_FILEMUSTEXIST, FOS_FORCEFILESYSTEM, FOS_NOCHANGEDIR, FOS_PATHMUSTEXIST, FileOpenDialog,
        SIGDN_FILESYSPATH,
    };
    use windows_sys::core::GUID;

    let started = unsafe { CoInitializeEx(std::ptr::null(), COINIT_APARTMENTTHREADED as u32) };
    // S_FALSE means this thread already initialized COM. RPC_E_CHANGED_MODE
    // means it is already initialized in another apartment. Only S_OK means
    // this call owns the initialization and must release it.
    let owns_com = started == 0;
    if started < 0 && started != RPC_E_CHANGED_MODE {
        return Err(PlatformError::DialogFailed);
    }

    let result = pick_with_com(kind);
    if owns_com {
        unsafe { CoUninitialize() };
    }

    fn pick_with_com(kind: OpenFileKind) -> Result<Option<PathBuf>, PlatformError> {
        let iid = GUID::from_u128(0xd57c_7288_d4ad_4768_be02_9d96_9532_d960);
        let mut dialog: *mut std::ffi::c_void = std::ptr::null_mut();
        let created = unsafe {
            CoCreateInstance(
                &FileOpenDialog,
                std::ptr::null_mut(),
                CLSCTX_INPROC_SERVER,
                &iid,
                &mut dialog,
            )
        };
        if created < 0 || dialog.is_null() {
            return Err(PlatformError::DialogFailed);
        }
        let chosen = show_open_dialog(dialog, kind);
        release(dialog);
        chosen
    }

    fn show_open_dialog(
        dialog: *mut std::ffi::c_void,
        kind: OpenFileKind,
    ) -> Result<Option<PathBuf>, PlatformError> {
        // SAFETY: `dialog` is a live IFileOpenDialog from CoCreateInstance. The
        // strings and filter stay alive for the calls below, and the vtable
        // slots match the Windows ABI of IFileOpenDialog.
        let vtbl = unsafe { *(dialog as *mut *const DialogVtbl) };
        let title = wide_null(match kind {
            OpenFileKind::Model => "モデルを開く",
            OpenFileKind::Motion => "モーションを開く",
            OpenFileKind::Audio => "音楽ファイルを開く",
        });
        let (label, spec) = match kind {
            OpenFileKind::Model => (wide_null("モデル (*.pmx;*.pmd)"), wide_null("*.pmx;*.pmd")),
            OpenFileKind::Motion => (wide_null("モーション (*.vmd)"), wide_null("*.vmd")),
            OpenFileKind::Audio => (
                wide_null("音楽ファイル (*.wav;*.mp3;*.m4a;*.ogg)"),
                wide_null("*.wav;*.mp3;*.m4a;*.ogg"),
            ),
        };
        let filters = [FilterSpec {
            name: label.as_ptr(),
            spec: spec.as_ptr(),
        }];
        let place = GUID::from_u128(dialog_place(kind));
        let options = FOS_FILEMUSTEXIST | FOS_PATHMUSTEXIST | FOS_NOCHANGEDIR | FOS_FORCEFILESYSTEM;
        let prepared = unsafe {
            ((*vtbl).set_file_types)(dialog, 1, filters.as_ptr()) < 0
                || ((*vtbl).set_options)(dialog, options) < 0
                || ((*vtbl).set_title)(dialog, title.as_ptr()) < 0
                || ((*vtbl).set_client_guid)(dialog, &place) < 0
        };
        if prepared {
            return Err(PlatformError::DialogFailed);
        }
        if unsafe { ((*vtbl).show)(dialog, std::ptr::null_mut()) } < 0 {
            return Ok(None);
        }
        let mut item: *mut std::ffi::c_void = std::ptr::null_mut();
        if unsafe { ((*vtbl).get_result)(dialog, &mut item) } < 0 || item.is_null() {
            return Ok(None);
        }
        let path = shell_path(item);
        release(item);
        path
    }

    fn shell_path(item: *mut std::ffi::c_void) -> Result<Option<PathBuf>, PlatformError> {
        // SAFETY: `item` is the IShellItem returned by GetResult. GetDisplayName
        // allocates the path with CoTaskMemAlloc, and this function frees it.
        let vtbl = unsafe { *(item as *mut *const ShellVtbl) };
        let mut name: *mut u16 = std::ptr::null_mut();
        let hr = unsafe { ((*vtbl).get_display_name)(item, SIGDN_FILESYSPATH, &mut name) };
        if hr < 0 || name.is_null() {
            return Ok(None);
        }
        let mut length = 0usize;
        while unsafe { *name.add(length) } != 0 {
            length += 1;
        }
        let path =
            std::ffi::OsString::from_wide(unsafe { std::slice::from_raw_parts(name, length) });
        unsafe { CoTaskMemFree(name.cast()) };
        Ok(Some(PathBuf::from(path)))
    }

    fn release(object: *mut std::ffi::c_void) {
        if object.is_null() {
            return;
        }
        unsafe {
            let release = (**object.cast::<*const ReleaseVtbl>()).release;
            release(object);
        }
    }

    result
}

#[cfg(windows)]
#[repr(C)]
struct FilterSpec {
    name: *const u16,
    spec: *const u16,
}

#[cfg(windows)]
#[repr(C)]
struct ReleaseVtbl {
    query_interface: *const (),
    add_ref: *const (),
    release: unsafe extern "system" fn(*mut std::ffi::c_void) -> u32,
}

#[cfg(windows)]
#[repr(C)]
struct DialogVtbl {
    query_interface: *const (),
    add_ref: *const (),
    release: unsafe extern "system" fn(*mut std::ffi::c_void) -> u32,
    show: unsafe extern "system" fn(*mut std::ffi::c_void, *mut std::ffi::c_void) -> i32,
    set_file_types: unsafe extern "system" fn(*mut std::ffi::c_void, u32, *const FilterSpec) -> i32,
    set_file_type_index: *const (),
    get_file_type_index: *const (),
    advise: *const (),
    unadvise: *const (),
    set_options: unsafe extern "system" fn(*mut std::ffi::c_void, u32) -> i32,
    get_options: *const (),
    set_default_folder: *const (),
    set_folder: *const (),
    get_folder: *const (),
    get_current_selection: *const (),
    set_file_name: *const (),
    get_file_name: *const (),
    set_title: unsafe extern "system" fn(*mut std::ffi::c_void, *const u16) -> i32,
    set_ok_button_label: *const (),
    set_file_name_label: *const (),
    get_result: unsafe extern "system" fn(*mut std::ffi::c_void, *mut *mut std::ffi::c_void) -> i32,
    add_place: *const (),
    set_default_extension: *const (),
    close: *const (),
    set_client_guid:
        unsafe extern "system" fn(*mut std::ffi::c_void, *const windows_sys::core::GUID) -> i32,
}

#[cfg(windows)]
#[repr(C)]
struct ShellVtbl {
    query_interface: *const (),
    add_ref: *const (),
    release: unsafe extern "system" fn(*mut std::ffi::c_void) -> u32,
    bind_to_handler: *const (),
    get_parent: *const (),
    get_display_name: unsafe extern "system" fn(*mut std::ffi::c_void, i32, *mut *mut u16) -> i32,
}

#[cfg(windows)]
const RPC_E_CHANGED_MODE: i32 = -2147417850;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wide_null_terminates_utf16() {
        let encoded = wide_null("MMD");
        assert_eq!(encoded, vec![b'M' as u16, b'M' as u16, b'D' as u16, 0]);
    }

    #[test]
    fn model_and_motion_dialogs_keep_separate_places() {
        assert_ne!(
            dialog_place(OpenFileKind::Model),
            dialog_place(OpenFileKind::Motion)
        );
    }

    #[test]
    fn wide_null_preserves_non_ascii_text() {
        let encoded = wide_null("ミク");
        assert_eq!(*encoded.last().expect("null terminator"), 0);
        assert!(encoded.len() > 1);
    }
}
