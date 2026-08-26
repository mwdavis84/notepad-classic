use std::cell::Cell;
use std::ffi::{OsStr, OsString, c_void};
use std::io;
use std::mem::{size_of, zeroed};
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};
use std::ptr::null_mut;

use windows_sys::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows_sys::Win32::Graphics::Gdi::{
    COLOR_BTNFACE, CreateFontIndirectW, DeleteObject, GetSysColorBrush, HFONT, LOGFONTW,
    UpdateWindow,
};
use windows_sys::Win32::UI::Controls::Dialogs::{
    CF_FORCEFONTEXIST, CF_INITTOLOGFONTSTRUCT, CF_SCREENFONTS, CHOOSEFONTW, ChooseFontW,
    CommDlgExtendedError, GetOpenFileNameW, GetSaveFileNameW, OFN_EXPLORER, OFN_FILEMUSTEXIST,
    OFN_HIDEREADONLY, OFN_NOCHANGEDIR, OFN_OVERWRITEPROMPT, OFN_PATHMUSTEXIST, OPENFILENAMEW,
};
use windows_sys::Win32::UI::Controls::{NM_CLICK, NM_RETURN, NMHDR, NMLINK};
use windows_sys::Win32::UI::HiDpi::{GetDpiForWindow, SystemParametersInfoForDpi};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{EnableWindow, SetFocus};
use windows_sys::Win32::UI::Shell::ShellExecuteW;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    BS_DEFPUSHBUTTON, CREATESTRUCTW, CS_DBLCLKS, CreateWindowExW, DefWindowProcW, DestroyIcon,
    DestroyWindow, DispatchMessageW, ES_AUTOHSCROLL, ES_NUMBER, EnumChildWindows, GWLP_HINSTANCE,
    GWLP_USERDATA, GetDlgItem, GetMessageW, GetWindowLongPtrW, GetWindowRect, GetWindowTextLengthW,
    GetWindowTextW, HICON, IDC_ARROW, IDCANCEL, IDNO, IDOK, IDYES, IMAGE_ICON, IsDialogMessageW,
    LR_DEFAULTCOLOR, LoadCursorW, LoadImageW, MB_ICONERROR, MB_ICONQUESTION, MB_ICONWARNING, MB_OK,
    MB_YESNO, MB_YESNOCANCEL, MSG, MessageBoxW, MoveWindow, NONCLIENTMETRICSW, PostQuitMessage,
    RegisterClassW, SPI_GETNONCLIENTMETRICS, STM_SETICON, SW_SHOW, SW_SHOWNORMAL, SWP_NOACTIVATE,
    SWP_NOZORDER, SendMessageW, SetForegroundWindow, SetWindowLongPtrW, SetWindowPos, ShowWindow,
    TranslateMessage, WINDOW_STYLE, WM_CLOSE, WM_COMMAND, WM_CREATE, WM_DESTROY, WM_DPICHANGED,
    WM_NCCREATE, WM_NCDESTROY, WM_NOTIFY, WM_SETFONT, WNDCLASSW, WS_BORDER, WS_CAPTION, WS_CHILD,
    WS_EX_CONTROLPARENT, WS_EX_DLGMODALFRAME, WS_POPUP, WS_SYSMENU, WS_TABSTOP, WS_VISIBLE,
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
        let wide = to_wide_os(path.as_os_str());
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
        return Ok(Some(PathBuf::from(OsString::from_wide(&buffer[..len]))));
    }
    let error = unsafe { CommDlgExtendedError() };
    if error == 0 {
        Ok(None)
    } else {
        Err(format!("The file dialog failed (error 0x{error:08X})."))
    }
}

pub fn choose_font(owner: HWND, current: &mut LOGFONTW) -> Option<i32> {
    let mut dialog: CHOOSEFONTW = unsafe { zeroed() };
    dialog.lStructSize = size_of::<CHOOSEFONTW>() as u32;
    dialog.hwndOwner = owner;
    dialog.lpLogFont = current;
    dialog.Flags = CF_SCREENFONTS | CF_INITTOLOGFONTSTRUCT | CF_FORCEFONTEXIST;
    // SAFETY: `current` is writable and lives through this synchronous dialog.
    if unsafe { ChooseFontW(&mut dialog) } != 0 {
        Some(dialog.iPointSize)
    } else {
        None
    }
}

pub fn confirm_save(owner: HWND, display_name: &OsStr) -> SaveDecision {
    let text = wide_with_os("Do you want to save changes to ", display_name, "?");
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

pub fn confirm_create(owner: HWND, path: &Path) -> bool {
    let text = wide_with_os(
        "Cannot find the ",
        path.as_os_str(),
        " file.\n\nDo you want to create a new file?",
    );
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

pub fn show_error_with_path(owner: Option<HWND>, title: &str, prefix: &str, path: &Path) {
    let title = to_wide(title);
    let message = wide_with_os(prefix, path.as_os_str(), "");
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

fn to_wide_os(text: &OsStr) -> Vec<u16> {
    text.encode_wide().chain(Some(0)).collect()
}

fn wide_with_os(prefix: &str, value: &OsStr, suffix: &str) -> Vec<u16> {
    prefix
        .encode_utf16()
        .chain(value.encode_wide())
        .chain(suffix.encode_utf16())
        .chain(Some(0))
        .collect()
}

struct GotoState {
    label: Cell<HWND>,
    edit: Cell<HWND>,
    ok: Cell<HWND>,
    cancel: Cell<HWND>,
    result: Cell<Option<u32>>,
    done: Cell<bool>,
    dpi: Cell<u32>,
    owned_font: Cell<HFONT>,
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

    let dpi = unsafe { GetDpiForWindow(owner) }.max(96);
    let mut state = Box::new(GotoState {
        label: Cell::new(null_mut()),
        edit: Cell::new(null_mut()),
        ok: Cell::new(null_mut()),
        cancel: Cell::new(null_mut()),
        result: Cell::new(None),
        done: Cell::new(false),
        dpi: Cell::new(dpi),
        owned_font: Cell::new(null_mut()),
        initial,
    });
    let mut owner_rect: RECT = unsafe { zeroed() };
    unsafe { GetWindowRect(owner, &mut owner_rect) };
    let width = scale_for_dpi(300, dpi);
    let height = scale_for_dpi(145, dpi);
    let x = owner_rect.left + ((owner_rect.right - owner_rect.left - width) / 2).max(0);
    let y = owner_rect.top + ((owner_rect.bottom - owner_rect.top - height) / 2).max(0);
    let title = to_wide("Go To Line");
    // End the temporary mutable borrow before `CreateWindowExW` can reenter the
    // dialog procedure with this raw pointer.
    let state_pointer = (&mut *state) as *mut GotoState;
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
            state_pointer.cast(),
        )
    };
    if hwnd.is_null() {
        return None;
    }
    unsafe {
        EnableWindow(owner, 0);
        ShowWindow(hwnd, SW_SHOW);
        UpdateWindow(hwnd);
        SetFocus(state.edit.get());
    }

    let mut message: MSG = unsafe { zeroed() };
    loop {
        // Closing the dialog destroys it synchronously from the current
        // dispatch, which returns here before the next `GetMessageW`.
        if state.done.get() {
            break;
        }
        let status = unsafe { GetMessageW(&mut message, null_mut(), 0, 0) };
        if status <= 0 {
            if status == 0 {
                unsafe { PostQuitMessage(message.wParam as i32) };
            }
            if !state.done.get() {
                state.done.set(true);
                unsafe { DestroyWindow(hwnd) };
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
    state.result.get()
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
    let state_pointer = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as *const GotoState;
    let Some(state) = (unsafe { state_pointer.as_ref() }) else {
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
            state.label.set(label);
            let initial = state.initial.to_string();
            state.edit.set(create_child(
                hwnd,
                "EDIT",
                &initial,
                WS_VISIBLE | WS_TABSTOP | WS_BORDER | ES_AUTOHSCROLL as u32 | ES_NUMBER as u32,
                12,
                34,
                260,
                24,
                100,
            ));
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
            state.ok.set(ok);
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
            state.cancel.set(cancel);
            layout_goto(state);
            replace_goto_font(state);
            0
        }
        WM_COMMAND => {
            match wparam & 0xFFFF {
                value if value == IDOK as usize => {
                    let edit = state.edit.get();
                    let length = unsafe { GetWindowTextLengthW(edit) };
                    let mut buffer = vec![0u16; length.max(0) as usize + 1];
                    let written =
                        unsafe { GetWindowTextW(edit, buffer.as_mut_ptr(), buffer.len() as i32) };
                    state.result.set(
                        String::from_utf16_lossy(&buffer[..written.max(0) as usize])
                            .trim()
                            .parse()
                            .ok(),
                    );
                    state.done.set(true);
                    unsafe { DestroyWindow(hwnd) };
                }
                value if value == IDCANCEL as usize => {
                    state.done.set(true);
                    unsafe { DestroyWindow(hwnd) };
                }
                _ => {}
            }
            0
        }
        WM_CLOSE => {
            state.done.set(true);
            unsafe { DestroyWindow(hwnd) };
            0
        }
        WM_DPICHANGED => {
            let suggested = unsafe { *(lparam as *const RECT) };
            let dpi = ((wparam >> 16) as u16 as u32).max(96);
            unsafe {
                SetWindowPos(
                    hwnd,
                    null_mut(),
                    suggested.left,
                    suggested.top,
                    suggested.right - suggested.left,
                    suggested.bottom - suggested.top,
                    SWP_NOZORDER | SWP_NOACTIVATE,
                );
            }
            state.dpi.set(dpi);
            layout_goto(state);
            replace_goto_font(state);
            0
        }
        WM_DESTROY => 0,
        WM_NCDESTROY => {
            unsafe { SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0) };
            state.label.set(null_mut());
            state.edit.set(null_mut());
            state.ok.set(null_mut());
            state.cancel.set(null_mut());
            state.done.set(true);
            let font = state.owned_font.replace(null_mut());
            if !font.is_null() {
                unsafe { DeleteObject(font) };
            }
            unsafe { DefWindowProcW(hwnd, message, wparam, lparam) }
        }
        _ => unsafe { DefWindowProcW(hwnd, message, wparam, lparam) },
    }
}

fn scale_for_dpi(value: i32, dpi: u32) -> i32 {
    ((value as i64 * dpi as i64 + 48) / 96) as i32
}

fn layout_goto(state: &GotoState) {
    let dpi = state.dpi.get();
    let controls = [
        (state.label.get(), 12, 12, 260, 20),
        (state.edit.get(), 12, 34, 260, 24),
        (state.ok.get(), 116, 70, 75, 26),
        (state.cancel.get(), 197, 70, 75, 26),
    ];
    for (control, x, y, width, height) in controls {
        if !control.is_null() {
            unsafe {
                MoveWindow(
                    control,
                    scale_for_dpi(x, dpi),
                    scale_for_dpi(y, dpi),
                    scale_for_dpi(width, dpi),
                    scale_for_dpi(height, dpi),
                    1,
                );
            }
        }
    }
}

fn create_goto_font(dpi: u32) -> HFONT {
    create_dialog_font(dpi)
}

fn create_dialog_font(dpi: u32) -> HFONT {
    let mut metrics: NONCLIENTMETRICSW = unsafe { zeroed() };
    metrics.cbSize = size_of::<NONCLIENTMETRICSW>() as u32;
    let succeeded = unsafe {
        SystemParametersInfoForDpi(
            SPI_GETNONCLIENTMETRICS,
            metrics.cbSize,
            (&mut metrics as *mut NONCLIENTMETRICSW).cast(),
            0,
            dpi,
        )
    };
    if succeeded == 0 {
        null_mut()
    } else {
        unsafe { CreateFontIndirectW(&metrics.lfMessageFont) }
    }
}

fn replace_goto_font(state: &GotoState) {
    let font = create_goto_font(state.dpi.get());
    if font.is_null() {
        return;
    }
    for child in [
        state.label.get(),
        state.edit.get(),
        state.ok.get(),
        state.cancel.get(),
    ] {
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
    if state.done.get() {
        unsafe { DeleteObject(font) };
        return;
    }
    let previous = state.owned_font.replace(font);
    if !previous.is_null() {
        unsafe { DeleteObject(previous) };
    }
}

const SS_ICON: u32 = 0x0000_0003;
const APP_ICON_RESOURCE_ID: usize = 1;
const ID_ABOUT_APP_NAME: usize = 1001;
const ID_ABOUT_PUBLISHER: usize = 1002;

struct AboutState {
    hwnd: Cell<HWND>,
    close_button: Cell<HWND>,
    icon_control: Cell<HWND>,
    link_controls: [Cell<HWND>; 3],
    owned_font: Cell<HFONT>,
    owned_icon: Cell<HICON>,
    done: Cell<bool>,
}

pub fn show_about(owner: HWND, instance: HINSTANCE) {
    let class_name = to_wide("NotepadClassicAboutDialog");
    let class = WNDCLASSW {
        style: 0,
        lpfnWndProc: Some(about_window_proc),
        hInstance: instance,
        hCursor: unsafe { LoadCursorW(null_mut(), IDC_ARROW) },
        hbrBackground: unsafe { GetSysColorBrush(COLOR_BTNFACE) },
        lpszClassName: class_name.as_ptr(),
        ..unsafe { zeroed() }
    };
    unsafe { RegisterClassW(&class) };

    let dpi = unsafe { GetDpiForWindow(owner) }.max(96);
    let mut state = Box::new(AboutState {
        hwnd: Cell::new(null_mut()),
        close_button: Cell::new(null_mut()),
        icon_control: Cell::new(null_mut()),
        link_controls: [
            Cell::new(null_mut()),
            Cell::new(null_mut()),
            Cell::new(null_mut()),
        ],
        owned_font: Cell::new(null_mut()),
        owned_icon: Cell::new(null_mut()),
        done: Cell::new(false),
    });

    unsafe { EnableWindow(owner, 0) };

    let mut owner_rect: RECT = unsafe { zeroed() };
    unsafe { GetWindowRect(owner, &mut owner_rect) };
    let width = scale_for_dpi(380, dpi);
    let height = scale_for_dpi(260, dpi);
    let x = owner_rect.left + ((owner_rect.right - owner_rect.left - width) / 2).max(0);
    let y = owner_rect.top + ((owner_rect.bottom - owner_rect.top - height) / 2).max(0);
    let title = to_wide("About Notepad Classic");
    let state_pointer = (&mut *state) as *mut AboutState;

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
            state_pointer.cast(),
        )
    };

    if hwnd.is_null() {
        unsafe {
            EnableWindow(owner, 1);
            SetForegroundWindow(owner);
        }
        return;
    }

    unsafe {
        ShowWindow(hwnd, SW_SHOW);
        UpdateWindow(hwnd);
        SetFocus(state.close_button.get());
    }

    let mut message: MSG = unsafe { zeroed() };
    loop {
        if state.done.get() {
            break;
        }
        let status = unsafe { GetMessageW(&mut message, null_mut(), 0, 0) };
        if status <= 0 {
            if status == 0 {
                unsafe { PostQuitMessage(message.wParam as i32) };
            }
            if !state.done.get() {
                state.done.set(true);
                unsafe { DestroyWindow(hwnd) };
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
}

unsafe extern "system" fn about_window_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if message == WM_NCCREATE {
        let create = lparam as *const CREATESTRUCTW;
        let state = unsafe { (*create).lpCreateParams.cast::<AboutState>() };
        unsafe { SetWindowLongPtrW(hwnd, GWLP_USERDATA, state as isize) };
        return 1;
    }
    let state_pointer = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as *const AboutState;
    let Some(state) = (unsafe { state_pointer.as_ref() }) else {
        return unsafe { DefWindowProcW(hwnd, message, wparam, lparam) };
    };

    match message {
        WM_CREATE => {
            state.hwnd.set(hwnd);
            let instance = unsafe { (*(lparam as *const CREATESTRUCTW)).hInstance };
            let dpi = unsafe { GetDpiForWindow(hwnd) }.max(96);

            let icon_control =
                create_child(hwnd, "STATIC", "", WS_VISIBLE | SS_ICON, 0, 0, 0, 0, 0);
            state.icon_control.set(icon_control);

            let app_version = format!("Notepad Classic\nVersion {}", env!("CARGO_PKG_VERSION"));
            let app_name_control = create_child(
                hwnd,
                "STATIC",
                &app_version,
                WS_VISIBLE,
                0,
                0,
                0,
                0,
                ID_ABOUT_APP_NAME,
            );

            let publisher_control = create_child(
                hwnd,
                "STATIC",
                "Publisher: DeekFit\nReleased under the MIT License.",
                WS_VISIBLE,
                0,
                0,
                0,
                0,
                ID_ABOUT_PUBLISHER,
            );

            let link1 = create_child(
                hwnd,
                "SysLink",
                "<a href=\"https://github.com/mwdavis84/notepad-classic\">GitHub Repository</a>",
                WS_VISIBLE | WS_TABSTOP,
                0,
                0,
                0,
                0,
                0,
            );
            let link2 = create_child(
                hwnd,
                "SysLink",
                "<a href=\"https://github.com/mwdavis84/notepad-classic/blob/main/LICENSE\">MIT License</a>",
                WS_VISIBLE | WS_TABSTOP,
                0,
                0,
                0,
                0,
                0,
            );
            let link3 = create_child(
                hwnd,
                "SysLink",
                "<a href=\"https://threads.com/@deekfit_apps\">Follow @deekfit_apps on Threads</a>",
                WS_VISIBLE | WS_TABSTOP,
                0,
                0,
                0,
                0,
                0,
            );
            state.link_controls[0].set(link1);
            state.link_controls[1].set(link2);
            state.link_controls[2].set(link3);

            let close_button = create_child(
                hwnd,
                "BUTTON",
                "Close",
                WS_VISIBLE | WS_TABSTOP | BS_DEFPUSHBUTTON as u32,
                0,
                0,
                0,
                0,
                IDCANCEL as usize,
            );
            state.close_button.set(close_button);

            if icon_control.is_null()
                || app_name_control.is_null()
                || publisher_control.is_null()
                || link1.is_null()
                || link2.is_null()
                || link3.is_null()
                || close_button.is_null()
            {
                return -1;
            }

            layout_about(state, dpi);
            replace_about_font(state, dpi);
            replace_about_icon(state, instance, dpi);
            0
        }
        WM_NOTIFY => {
            let nmhdr = unsafe { &*(lparam as *const NMHDR) };
            if nmhdr.code == NM_CLICK || nmhdr.code == NM_RETURN {
                let nmlink = unsafe { &*(lparam as *const NMLINK) };
                let url = &nmlink.item.szUrl;
                let result = unsafe {
                    ShellExecuteW(
                        hwnd,
                        to_wide("open").as_ptr(),
                        url.as_ptr(),
                        null_mut(),
                        null_mut(),
                        SW_SHOWNORMAL,
                    )
                };
                if (result as isize) <= 32 {
                    show_error(Some(hwnd), "Notepad Classic", "Unable to open the link.");
                }
                0
            } else {
                unsafe { DefWindowProcW(hwnd, message, wparam, lparam) }
            }
        }
        WM_COMMAND => {
            let id = (wparam & 0xFFFF) as i32;
            if id == IDOK || id == IDCANCEL {
                state.done.set(true);
                unsafe { DestroyWindow(hwnd) };
            }
            0
        }
        WM_CLOSE => {
            state.done.set(true);
            unsafe { DestroyWindow(hwnd) };
            0
        }
        WM_DPICHANGED => {
            let suggested = unsafe { *(lparam as *const RECT) };
            let dpi = ((wparam >> 16) as u16 as u32).max(96);
            unsafe {
                SetWindowPos(
                    hwnd,
                    null_mut(),
                    suggested.left,
                    suggested.top,
                    suggested.right - suggested.left,
                    suggested.bottom - suggested.top,
                    SWP_NOZORDER | SWP_NOACTIVATE,
                );
            }
            layout_about(state, dpi);
            replace_about_font(state, dpi);
            let instance = unsafe { GetWindowLongPtrW(hwnd, GWLP_HINSTANCE) as HINSTANCE };
            replace_about_icon(state, instance, dpi);
            0
        }
        WM_NCDESTROY => {
            unsafe { SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0) };
            state.hwnd.set(null_mut());
            state.close_button.set(null_mut());
            state.icon_control.set(null_mut());
            state.link_controls[0].set(null_mut());
            state.link_controls[1].set(null_mut());
            state.link_controls[2].set(null_mut());
            state.done.set(true);
            let font = state.owned_font.replace(null_mut());
            if !font.is_null() {
                unsafe { DeleteObject(font) };
            }
            let icon = state.owned_icon.replace(null_mut());
            if !icon.is_null() {
                unsafe { DestroyIcon(icon) };
            }
            unsafe { DefWindowProcW(hwnd, message, wparam, lparam) }
        }
        _ => unsafe { DefWindowProcW(hwnd, message, wparam, lparam) },
    }
}

fn layout_about(state: &AboutState, dpi: u32) {
    let hwnd = state.hwnd.get();
    let app_name = unsafe { GetDlgItem(hwnd, ID_ABOUT_APP_NAME as i32) };
    let publisher = unsafe { GetDlgItem(hwnd, ID_ABOUT_PUBLISHER as i32) };
    let icon_size = scale_for_dpi(32, dpi);

    let controls = [
        (
            state.icon_control.get(),
            scale_for_dpi(20, dpi),
            scale_for_dpi(20, dpi),
            icon_size,
            icon_size,
        ),
        (
            app_name,
            scale_for_dpi(68, dpi),
            scale_for_dpi(16, dpi),
            scale_for_dpi(280, dpi),
            scale_for_dpi(34, dpi),
        ),
        (
            publisher,
            scale_for_dpi(68, dpi),
            scale_for_dpi(56, dpi),
            scale_for_dpi(280, dpi),
            scale_for_dpi(34, dpi),
        ),
        (
            state.link_controls[0].get(),
            scale_for_dpi(68, dpi),
            scale_for_dpi(96, dpi),
            scale_for_dpi(280, dpi),
            scale_for_dpi(18, dpi),
        ),
        (
            state.link_controls[1].get(),
            scale_for_dpi(68, dpi),
            scale_for_dpi(118, dpi),
            scale_for_dpi(280, dpi),
            scale_for_dpi(18, dpi),
        ),
        (
            state.link_controls[2].get(),
            scale_for_dpi(68, dpi),
            scale_for_dpi(140, dpi),
            scale_for_dpi(280, dpi),
            scale_for_dpi(18, dpi),
        ),
        (
            state.close_button.get(),
            scale_for_dpi(275, dpi),
            scale_for_dpi(172, dpi),
            scale_for_dpi(75, dpi),
            scale_for_dpi(26, dpi),
        ),
    ];

    for (control, x, y, width, height) in controls {
        if !control.is_null() {
            unsafe {
                MoveWindow(control, x, y, width, height, 1);
            }
        }
    }
}

unsafe extern "system" fn set_child_font(child: HWND, lparam: LPARAM) -> i32 {
    unsafe {
        SendMessageW(child, WM_SETFONT, lparam as usize, 1);
    }
    1
}

fn replace_about_font(state: &AboutState, dpi: u32) {
    let font = create_dialog_font(dpi);
    if font.is_null() {
        return;
    }
    let hwnd = state.hwnd.get();
    if !hwnd.is_null() {
        unsafe {
            EnumChildWindows(hwnd, Some(set_child_font), font as isize);
        }
    }
    if state.done.get() {
        unsafe { DeleteObject(font) };
        return;
    }
    let previous = state.owned_font.replace(font);
    if !previous.is_null() {
        unsafe { DeleteObject(previous) };
    }
}

fn replace_about_icon(state: &AboutState, instance: HINSTANCE, dpi: u32) {
    let size = scale_for_dpi(32, dpi).max(1);
    let icon = unsafe {
        LoadImageW(
            instance,
            APP_ICON_RESOURCE_ID as *const u16,
            IMAGE_ICON,
            size,
            size,
            LR_DEFAULTCOLOR,
        ) as HICON
    };
    if icon.is_null() {
        return;
    }
    let icon_control = state.icon_control.get();
    if !icon_control.is_null() {
        unsafe {
            SendMessageW(icon_control, STM_SETICON, icon as usize, 0);
        }
    }
    if state.done.get() {
        unsafe { DestroyIcon(icon) };
        return;
    }
    let previous = state.owned_icon.replace(icon);
    if !previous.is_null() {
        unsafe { DestroyIcon(previous) };
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_os_strings_round_trip_without_unicode_loss() {
        let units = [b'C' as u16, b':' as u16, b'\\' as u16, 0xD800, b'x' as u16];
        let value = OsString::from_wide(&units);
        let wide = to_wide_os(&value);

        assert_eq!(&wide[..wide.len() - 1], &units);
        assert_eq!(wide.last(), Some(&0));
        let round_trip = OsString::from_wide(&wide[..wide.len() - 1]);
        assert_eq!(round_trip.encode_wide().collect::<Vec<_>>(), units);
    }

    #[test]
    fn scale_for_dpi_scales_proportionally() {
        // 96 DPI (100% scale)
        assert_eq!(scale_for_dpi(380, 96), 380);
        assert_eq!(scale_for_dpi(260, 96), 260);
        assert_eq!(scale_for_dpi(32, 96), 32);

        // 120 DPI (125% scale)
        assert_eq!(scale_for_dpi(380, 120), 475);
        assert_eq!(scale_for_dpi(260, 120), 325);
        assert_eq!(scale_for_dpi(32, 120), 40);

        // 144 DPI (150% scale)
        assert_eq!(scale_for_dpi(380, 144), 570);
        assert_eq!(scale_for_dpi(260, 144), 390);
        assert_eq!(scale_for_dpi(32, 144), 48);

        // 192 DPI (200% scale)
        assert_eq!(scale_for_dpi(380, 192), 760);
        assert_eq!(scale_for_dpi(260, 192), 520);
        assert_eq!(scale_for_dpi(32, 192), 64);
    }

    #[test]
    fn about_dialog_hyperlinks_are_valid_syslink_markup() {
        let links = [
            "<a href=\"https://github.com/mwdavis84/notepad-classic\">GitHub Repository</a>",
            "<a href=\"https://github.com/mwdavis84/notepad-classic/blob/main/LICENSE\">MIT License</a>",
            "<a href=\"https://threads.com/@deekfit_apps\">Follow @deekfit_apps on Threads</a>",
        ];

        for link in links {
            assert!(link.starts_with("<a href=\"https://"));
            assert!(link.ends_with("</a>"));
            assert!(link.contains("\">"));
        }
    }

    #[test]
    fn about_dialog_app_version_string_matches_package() {
        let app_version = format!("Notepad Classic\nVersion {}", env!("CARGO_PKG_VERSION"));
        assert!(app_version.starts_with("Notepad Classic\nVersion "));
        assert!(!env!("CARGO_PKG_VERSION").is_empty());
    }
}
