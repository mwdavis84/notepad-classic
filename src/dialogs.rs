use std::ffi::c_void;
use std::io;
use std::mem::{size_of, zeroed};
use std::path::{Path, PathBuf};
use std::ptr::null_mut;

use windows_sys::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows_sys::Win32::Graphics::Gdi::{
    COLOR_BTNFACE, DEFAULT_GUI_FONT, GetStockObject, GetSysColorBrush, LOGFONTW, UpdateWindow,
};
use windows_sys::Win32::UI::Controls::Dialogs::{
    CF_FORCEFONTEXIST, CF_INITTOLOGFONTSTRUCT, CF_SCREENFONTS, CHOOSEFONTW, ChooseFontW,
    CommDlgExtendedError, GetOpenFileNameW, GetSaveFileNameW, OFN_EXPLORER, OFN_FILEMUSTEXIST,
    OFN_HIDEREADONLY, OFN_NOCHANGEDIR, OFN_OVERWRITEPROMPT, OFN_PATHMUSTEXIST, OPENFILENAMEW,
};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{EnableWindow, SetFocus};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    BS_DEFPUSHBUTTON, CREATESTRUCTW, CS_DBLCLKS, CreateWindowExW, DefWindowProcW, DestroyWindow,
    DispatchMessageW, ES_AUTOHSCROLL, ES_NUMBER, GWLP_USERDATA, GetMessageW, GetWindowLongPtrW,
    GetWindowRect, GetWindowTextLengthW, GetWindowTextW, IDC_ARROW, IDCANCEL, IDNO, IDOK, IDYES,
    IsDialogMessageW, LoadCursorW, MB_ICONERROR, MB_ICONQUESTION, MB_ICONWARNING, MB_OK, MB_YESNO,
    MB_YESNOCANCEL, MSG, MessageBoxW, PostQuitMessage, RegisterClassW, SW_SHOW,
    SetForegroundWindow, SetWindowLongPtrW, ShowWindow, TranslateMessage, WINDOW_STYLE, WM_CLOSE,
    WM_COMMAND, WM_CREATE, WM_DESTROY, WM_NCCREATE, WM_SETFONT, WNDCLASSW, WS_BORDER, WS_CAPTION,
    WS_CHILD, WS_EX_CONTROLPARENT, WS_EX_DLGMODALFRAME, WS_POPUP, WS_SYSMENU, WS_TABSTOP,
    WS_VISIBLE,
};

const FILE_BUFFER_LEN: usize = 32_768;
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SaveDecision {
    Save,
    Discard,
    Cancel,
}

pub fn open_file(owner: HWND) -> Result<Option<PathBuf>, String> {
    file_dialog(owner, None, false)
}

pub fn save_file(owner: HWND, current: Option<&Path>) -> Result<Option<PathBuf>, String> {
    file_dialog(owner, current, true)
}

fn file_dialog(owner: HWND, current: Option<&Path>, save: bool) -> Result<Option<PathBuf>, String> {
    let mut buffer = vec![0u16; FILE_BUFFER_LEN];
    let filter = to_wide("Text Documents (*.txt)\0*.txt\0All Files (*.*)\0*.*\0");
    if let Some(path) = current {
        let wide = to_wide(path.as_os_str().to_string_lossy().as_ref());
        let count = wide.len().min(buffer.len() - 1);
        buffer[..count].copy_from_slice(&wide[..count]);
    }
    let default_extension = to_wide("txt");
    let title = to_wide(if save { "Save As" } else { "Open" });
    let mut dialog: OPENFILENAMEW = unsafe { zeroed() };
    dialog.lStructSize = size_of::<OPENFILENAMEW>() as u32;
    dialog.hwndOwner = owner;
    dialog.lpstrFilter = filter.as_ptr();
    dialog.nFilterIndex = 1;
    dialog.lpstrFile = buffer.as_mut_ptr();
    dialog.nMaxFile = buffer.len() as u32;
    dialog.lpstrTitle = title.as_ptr();
    dialog.lpstrDefExt = default_extension.as_ptr();
    dialog.Flags = OFN_EXPLORER
        | OFN_HIDEREADONLY
        | OFN_NOCHANGEDIR
        | OFN_PATHMUSTEXIST
        | if save {
            OFN_OVERWRITEPROMPT
        } else {
            OFN_FILEMUSTEXIST
        };

    // SAFETY: every pointer in `dialog` refers to storage alive for the call;
    // the output buffer is writable and its capacity is reported accurately.
    let accepted = unsafe {
        if save {
            GetSaveFileNameW(&mut dialog)
        } else {
            GetOpenFileNameW(&mut dialog)
        }
    };
    if accepted != 0 {
        let len = buffer
            .iter()
            .position(|&unit| unit == 0)
            .unwrap_or(buffer.len());
        return Ok(Some(PathBuf::from(String::from_utf16_lossy(
            &buffer[..len],
        ))));
    }
    let error = unsafe { CommDlgExtendedError() };
    if error == 0 {
        Ok(None)
    } else {
        Err(format!("The file dialog failed (error 0x{error:08X})."))
    }
}

pub fn choose_font(owner: HWND, current: &mut LOGFONTW) -> bool {
    let mut dialog: CHOOSEFONTW = unsafe { zeroed() };
    dialog.lStructSize = size_of::<CHOOSEFONTW>() as u32;
    dialog.hwndOwner = owner;
    dialog.lpLogFont = current;
    dialog.Flags = CF_SCREENFONTS | CF_INITTOLOGFONTSTRUCT | CF_FORCEFONTEXIST;
    // SAFETY: `current` is writable and lives through this synchronous dialog.
    unsafe { ChooseFontW(&mut dialog) != 0 }
}

pub fn confirm_save(owner: HWND, display_name: &str) -> SaveDecision {
    let text = to_wide(&format!("Do you want to save changes to {display_name}?"));
    let title = to_wide("Notepad Classic");
    let answer = unsafe {
        MessageBoxW(
            owner,
            text.as_ptr(),
            title.as_ptr(),
            MB_YESNOCANCEL | MB_ICONWARNING,
        )
    };
    match answer {
        IDYES => SaveDecision::Save,
        IDNO => SaveDecision::Discard,
        IDCANCEL => SaveDecision::Cancel,
        _ => SaveDecision::Cancel,
    }
}

pub fn confirm_create(owner: HWND, path: &str) -> bool {
    let text = to_wide(&format!(
        "Cannot find the {path} file.\n\nDo you want to create a new file?"
    ));
    let title = to_wide("Notepad Classic");
    unsafe {
        MessageBoxW(
            owner,
            text.as_ptr(),
            title.as_ptr(),
            MB_YESNO | MB_ICONQUESTION,
        ) == IDYES
    }
}

pub fn show_error(owner: Option<HWND>, title: &str, message: &str) {
    let title = to_wide(title);
    let message = to_wide(message);
    unsafe {
        MessageBoxW(
            owner.unwrap_or(null_mut()),
            message.as_ptr(),
            title.as_ptr(),
            MB_OK | MB_ICONERROR,
        );
    }
}

pub fn os_error(context: &str) -> String {
    let error = io::Error::last_os_error();
    if error.raw_os_error() == Some(0) {
        context.to_owned()
    } else {
        format!("{context}: {error}")
    }
}

pub fn to_wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(Some(0)).collect()
}

struct GotoState {
    edit: HWND,
    result: Option<u32>,
    done: bool,
    initial: u32,
}

pub fn go_to_line(owner: HWND, instance: HINSTANCE, initial: u32) -> Option<u32> {
    let class_name = to_wide("NotepadClassicGotoDialog");
    let class = WNDCLASSW {
        style: CS_DBLCLKS,
        lpfnWndProc: Some(goto_window_proc),
        hInstance: instance,
        hCursor: unsafe { LoadCursorW(null_mut(), IDC_ARROW) },
        hbrBackground: unsafe { GetSysColorBrush(COLOR_BTNFACE) },
        lpszClassName: class_name.as_ptr(),
        ..unsafe { zeroed() }
    };
    // Registration returning zero is harmless after the first invocation: the
    // same process owns the already-registered class.
    unsafe { RegisterClassW(&class) };

    let mut state = Box::new(GotoState {
        edit: null_mut(),
        result: None,
        done: false,
        initial,
    });
    let mut owner_rect: RECT = unsafe { zeroed() };
    unsafe { GetWindowRect(owner, &mut owner_rect) };
    let width = 300;
    let height = 145;
    let x = owner_rect.left + ((owner_rect.right - owner_rect.left - width) / 2).max(0);
    let y = owner_rect.top + ((owner_rect.bottom - owner_rect.top - height) / 2).max(0);
    let title = to_wide("Go To Line");
    let hwnd = unsafe {
        CreateWindowExW(
            WS_EX_DLGMODALFRAME | WS_EX_CONTROLPARENT,
            class_name.as_ptr(),
            title.as_ptr(),
            WS_POPUP | WS_CAPTION | WS_SYSMENU,
            x,
            y,
            width,
            height,
            owner,
            null_mut(),
            instance,
            (&mut *state as *mut GotoState).cast(),
        )
    };
    if hwnd.is_null() {
        return None;
    }
    unsafe {
        EnableWindow(owner, 0);
        ShowWindow(hwnd, SW_SHOW);
        UpdateWindow(hwnd);
        SetFocus(state.edit);
    }

    let mut message: MSG = unsafe { zeroed() };
    loop {
        if state.done {
            break;
        }
        let status = unsafe { GetMessageW(&mut message, null_mut(), 0, 0) };
        if status <= 0 {
            if status == 0 {
                unsafe { PostQuitMessage(message.wParam as i32) };
            }
            break;
        }
        if unsafe { IsDialogMessageW(hwnd, &message) } == 0 {
            unsafe {
                TranslateMessage(&message);
                DispatchMessageW(&message);
            }
        }
    }
    unsafe {
        EnableWindow(owner, 1);
        SetForegroundWindow(owner);
    }
    state.result
}

unsafe extern "system" fn goto_window_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if message == WM_NCCREATE {
        let create = lparam as *const CREATESTRUCTW;
        let state = unsafe { (*create).lpCreateParams.cast::<GotoState>() };
        unsafe { SetWindowLongPtrW(hwnd, GWLP_USERDATA, state as isize) };
        return 1;
    }
    let state_pointer = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as *mut GotoState;
    let Some(state) = (unsafe { state_pointer.as_mut() }) else {
        return unsafe { DefWindowProcW(hwnd, message, wparam, lparam) };
    };
    match message {
        WM_CREATE => {
            let label = create_child(
                hwnd,
                "STATIC",
                "&Line number:",
                WS_VISIBLE,
                12,
                12,
                260,
                20,
                0,
            );
            let initial = state.initial.to_string();
            state.edit = create_child(
                hwnd,
                "EDIT",
                &initial,
                WS_VISIBLE | WS_TABSTOP | WS_BORDER | ES_AUTOHSCROLL as u32 | ES_NUMBER as u32,
                12,
                34,
                260,
                24,
                100,
            );
            let ok = create_child(
                hwnd,
                "BUTTON",
                "OK",
                WS_VISIBLE | WS_TABSTOP | BS_DEFPUSHBUTTON as u32,
                116,
                70,
                75,
                26,
                IDOK as usize,
            );
            let cancel = create_child(
                hwnd,
                "BUTTON",
                "Cancel",
                WS_VISIBLE | WS_TABSTOP,
                197,
                70,
                75,
                26,
                IDCANCEL as usize,
            );
            let font = unsafe { GetStockObject(DEFAULT_GUI_FONT) };
            for child in [label, state.edit, ok, cancel] {
                if !child.is_null() {
                    unsafe {
                        windows_sys::Win32::UI::WindowsAndMessaging::SendMessageW(
                            child,
                            WM_SETFONT,
                            font as usize,
                            1,
                        )
                    };
                }
            }
            0
        }
        WM_COMMAND => {
            match wparam & 0xFFFF {
                value if value == IDOK as usize => {
                    let length = unsafe { GetWindowTextLengthW(state.edit) };
                    let mut buffer = vec![0u16; length.max(0) as usize + 1];
                    let written = unsafe {
                        GetWindowTextW(state.edit, buffer.as_mut_ptr(), buffer.len() as i32)
                    };
                    state.result = String::from_utf16_lossy(&buffer[..written.max(0) as usize])
                        .trim()
                        .parse()
                        .ok();
                    state.done = true;
                    unsafe { DestroyWindow(hwnd) };
                }
                value if value == IDCANCEL as usize => {
                    state.done = true;
                    unsafe { DestroyWindow(hwnd) };
                }
                _ => {}
            }
            0
        }
        WM_CLOSE => {
            state.done = true;
            unsafe { DestroyWindow(hwnd) };
            0
        }
        WM_DESTROY => 0,
        _ => unsafe { DefWindowProcW(hwnd, message, wparam, lparam) },
    }
}

#[allow(clippy::too_many_arguments)]
fn create_child(
    parent: HWND,
    class: &str,
    text: &str,
    style: WINDOW_STYLE,
    x: i32,
    y: i32,
    width: i32,
    height: i32,
    id: usize,
) -> HWND {
    let class = to_wide(class);
    let text = to_wide(text);
    unsafe {
        CreateWindowExW(
            0,
            class.as_ptr(),
            text.as_ptr(),
            WS_CHILD | style,
            x,
            y,
            width,
            height,
            parent,
            id as *mut c_void,
            null_mut(),
            null_mut(),
        )
    }
}
