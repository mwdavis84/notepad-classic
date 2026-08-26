use std::ffi::c_void;
use std::mem::{size_of, zeroed};
use std::path::{Path, PathBuf};
use std::ptr::{null, null_mut};

use windows_sys::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, RECT, SYSTEMTIME, WPARAM};
use windows_sys::Win32::Globalization::{
    CSTR_EQUAL, CompareStringOrdinal, GetDateFormatEx, GetTimeFormatEx,
};
use windows_sys::Win32::Graphics::Gdi::{
    CLEARTYPE_QUALITY, COLOR_WINDOW, CreateFontIndirectW, DEFAULT_CHARSET, DEFAULT_GUI_FONT,
    DeleteObject, FF_MODERN, FIXED_PITCH, FW_NORMAL, GetObjectW, GetStockObject, GetSysColorBrush,
    HFONT, LOGFONTW, UpdateWindow,
};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::System::SystemInformation::GetLocalTime;
use windows_sys::Win32::System::SystemServices::MK_LBUTTON;
use windows_sys::Win32::UI::Controls::Dialogs::{
    FINDMSGSTRINGW, FINDREPLACEW, FR_DIALOGTERM, FR_DOWN, FR_FINDNEXT, FR_MATCHCASE, FR_REPLACE,
    FR_REPLACEALL, FR_WHOLEWORD, FindTextW, ReplaceTextW,
};
use windows_sys::Win32::UI::Controls::{
    EM_GETLINECOUNT, EM_GETSEL, EM_LINEFROMCHAR, EM_LINEINDEX, EM_REPLACESEL, EM_SCROLLCARET,
    EM_SETLIMITTEXT, EM_SETMODIFY, EM_SETSEL, ICC_BAR_CLASSES, INITCOMMONCONTROLSEX,
    InitCommonControlsEx, SB_SETTEXTW, STATUSCLASSNAMEW,
};
use windows_sys::Win32::UI::HiDpi::{
    DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, GetDpiForWindow, SetProcessDpiAwarenessContext,
};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{SetFocus, VK_DELETE, VK_F3, VK_F5};
use windows_sys::Win32::UI::Shell::{DragAcceptFiles, DragFinish, DragQueryFileW, HDROP};
use windows_sys::Win32::UI::WindowsAndMessaging::*;

use crate::dialogs::{self, SaveDecision};
use crate::file::{self, TextFormat};

const APP_NAME: &str = "Notepad Classic";
const CLASS_NAME: &[u16] = &[
    b'N' as u16,
    b'o' as u16,
    b't' as u16,
    b'e' as u16,
    b'p' as u16,
    b'a' as u16,
    b'd' as u16,
    b'C' as u16,
    b'l' as u16,
    b'a' as u16,
    b's' as u16,
    b's' as u16,
    b'i' as u16,
    b'c' as u16,
    0,
];

const ID_EDITOR: usize = 100;
const ID_STATUS: usize = 101;
const ID_FILE_NEW: usize = 1000;
const ID_FILE_OPEN: usize = 1001;
const ID_FILE_SAVE: usize = 1002;
const ID_FILE_SAVE_AS: usize = 1003;
const ID_FILE_EXIT: usize = 1004;
const ID_EDIT_UNDO: usize = 1100;
const ID_EDIT_CUT: usize = 1101;
const ID_EDIT_COPY: usize = 1102;
const ID_EDIT_PASTE: usize = 1103;
const ID_EDIT_DELETE: usize = 1104;
const ID_EDIT_FIND: usize = 1105;
const ID_EDIT_FIND_NEXT: usize = 1106;
const ID_EDIT_REPLACE: usize = 1107;
const ID_EDIT_GOTO: usize = 1108;
const ID_EDIT_SELECT_ALL: usize = 1109;
const ID_EDIT_TIME_DATE: usize = 1110;
const ID_EDIT_FIND_PREVIOUS: usize = 1111;
const ID_FORMAT_WRAP: usize = 1200;
const ID_FORMAT_FONT: usize = 1201;
const ID_VIEW_STATUS: usize = 1300;
const WM_APP_UPDATE_STATUS: u32 = WM_APP + 1;
const FIND_OPTION_FLAGS: u32 = FR_DOWN | FR_MATCHCASE | FR_WHOLEWORD;

struct AppState {
    instance: HINSTANCE,
    hwnd: HWND,
    editor: HWND,
    status: HWND,
    menu: HMENU,
    path: Option<PathBuf>,
    format: TextFormat,
    dirty: bool,
    suppress_change: bool,
    word_wrap: bool,
    status_requested: bool,
    owned_font: HFONT,
    find_message: u32,
    find_dialog: HWND,
    find_data: Option<Box<FINDREPLACEW>>,
    find_flags: u32,
    find_text: Box<[u16; 256]>,
    replace_text: Box<[u16; 256]>,
}

impl AppState {
    fn new(instance: HINSTANCE, find_message: u32) -> Self {
        Self {
            instance,
            hwnd: null_mut(),
            editor: null_mut(),
            status: null_mut(),
            menu: null_mut(),
            path: None,
            format: TextFormat::default(),
            dirty: false,
            suppress_change: false,
            word_wrap: false,
            status_requested: true,
            owned_font: null_mut(),
            find_message,
            find_dialog: null_mut(),
            find_data: None,
            find_flags: FR_DOWN,
            find_text: Box::new([0; 256]),
            replace_text: Box::new([0; 256]),
        }
    }

    fn display_name(&self) -> String {
        self.path
            .as_deref()
            .and_then(Path::file_name)
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| "Untitled".to_owned())
    }

    fn set_title(&self) {
        let marker = if self.dirty { "*" } else { "" };
        let title = dialogs::to_wide(&format!("{marker}{} - {APP_NAME}", self.display_name()));
        unsafe { SetWindowTextW(self.hwnd, title.as_ptr()) };
    }

    fn status_is_visible(&self) -> bool {
        self.status_requested && !self.word_wrap
    }
}

pub fn run() -> Result<(), String> {
    unsafe {
        // Failure only means an older Windows DPI mode remains in effect.
        SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    }
    let controls = INITCOMMONCONTROLSEX {
        dwSize: size_of::<INITCOMMONCONTROLSEX>() as u32,
        dwICC: ICC_BAR_CLASSES,
    };
    unsafe { InitCommonControlsEx(&controls) };

    let instance = unsafe { GetModuleHandleW(null()) };
    if instance.is_null() {
        return Err(dialogs::os_error("Unable to get the application module"));
    }
    let find_message = unsafe { RegisterWindowMessageW(FINDMSGSTRINGW) };
    if find_message == 0 {
        return Err(dialogs::os_error("Unable to register the Find message"));
    }

    let icon = unsafe { LoadIconW(null_mut(), IDI_APPLICATION) };
    let cursor = unsafe { LoadCursorW(null_mut(), IDC_IBEAM) };
    let class = WNDCLASSEXW {
        cbSize: size_of::<WNDCLASSEXW>() as u32,
        style: CS_HREDRAW | CS_VREDRAW,
        lpfnWndProc: Some(window_proc),
        cbClsExtra: 0,
        cbWndExtra: 0,
        hInstance: instance,
        hIcon: icon,
        hCursor: cursor,
        hbrBackground: unsafe { GetSysColorBrush(COLOR_WINDOW) },
        lpszMenuName: null(),
        lpszClassName: CLASS_NAME.as_ptr(),
        hIconSm: icon,
    };
    if unsafe { RegisterClassExW(&class) } == 0 {
        return Err(dialogs::os_error("Unable to register the window class"));
    }

    let menu = create_menu()?;
    let mut state = Box::new(AppState::new(instance, find_message));
    state.menu = menu;
    let state_pointer = Box::into_raw(state);
    let title = dialogs::to_wide("Untitled - Notepad Classic");
    let hwnd = unsafe {
        CreateWindowExW(
            0,
            CLASS_NAME.as_ptr(),
            title.as_ptr(),
            WS_OVERLAPPEDWINDOW | WS_CLIPCHILDREN,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            800,
            600,
            null_mut(),
            menu,
            instance,
            state_pointer.cast(),
        )
    };
    if hwnd.is_null() {
        unsafe { drop(Box::from_raw(state_pointer)) };
        return Err(dialogs::os_error("Unable to create the main window"));
    }

    unsafe {
        ShowWindow(hwnd, SW_SHOW);
        UpdateWindow(hwnd);
    }

    if let Some(argument) = std::env::args_os().nth(1) {
        let path = PathBuf::from(argument);
        unsafe {
            if let Some(state) = state_from_hwnd(hwnd) {
                open_command_line_path(state, path);
            }
        }
    }

    let accelerator = create_accelerators()?;
    let mut message: MSG = unsafe { zeroed() };
    loop {
        let result = unsafe { GetMessageW(&mut message, null_mut(), 0, 0) };
        if result == -1 {
            unsafe { DestroyAcceleratorTable(accelerator) };
            return Err(dialogs::os_error("The message loop failed"));
        }
        if result == 0 {
            break;
        }

        let find_dialog = unsafe {
            state_from_hwnd(hwnd)
                .map(|state| state.find_dialog)
                .unwrap_or(null_mut())
        };
        if !find_dialog.is_null() && unsafe { IsDialogMessageW(find_dialog, &message) } != 0 {
            continue;
        }
        if unsafe { TranslateAcceleratorW(hwnd, accelerator, &message) } != 0 {
            continue;
        }
        let update_after = message.message == WM_KEYDOWN
            || message.message == WM_KEYUP
            || message.message == WM_LBUTTONUP
            || message.message == WM_RBUTTONUP
            || (message.message == WM_MOUSEMOVE && message.wParam & MK_LBUTTON as usize != 0);
        unsafe {
            TranslateMessage(&message);
            DispatchMessageW(&message);
            if update_after && GetParent(message.hwnd) == hwnd {
                SendMessageW(hwnd, WM_APP_UPDATE_STATUS, 0, 0);
            }
        }
    }
    unsafe { DestroyAcceleratorTable(accelerator) };
    Ok(())
}

unsafe extern "system" fn window_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if message == WM_NCCREATE {
        let create = lparam as *const CREATESTRUCTW;
        let state = unsafe { (*create).lpCreateParams.cast::<AppState>() };
        unsafe {
            (*state).hwnd = hwnd;
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, state as isize);
        }
        return 1;
    }

    let Some(state) = (unsafe { state_from_hwnd(hwnd) }) else {
        return unsafe { DefWindowProcW(hwnd, message, wparam, lparam) };
    };

    if message == state.find_message {
        handle_find_message(state, lparam as *const FINDREPLACEW);
        return 0;
    }

    match message {
        WM_CREATE => match create_children(state) {
            Ok(()) => {
                unsafe { DragAcceptFiles(hwnd, 1) };
                state.set_title();
                0
            }
            Err(message) => {
                dialogs::show_error(Some(hwnd), APP_NAME, &message);
                -1
            }
        },
        WM_SIZE => {
            layout_children(state);
            0
        }
        WM_SETFOCUS => {
            unsafe { SetFocus(state.editor) };
            0
        }
        WM_COMMAND => {
            let id = wparam & 0xFFFF;
            let notification = (wparam >> 16) & 0xFFFF;
            if id == ID_EDITOR && notification == EN_CHANGE as usize {
                if !state.suppress_change {
                    state.dirty = true;
                    state.set_title();
                }
                update_status(state);
            } else {
                handle_command(state, id);
            }
            0
        }
        WM_DROPFILES => {
            handle_drop(state, wparam as HDROP);
            0
        }
        WM_CLOSE => {
            if maybe_save(state) {
                unsafe { DestroyWindow(hwnd) };
            }
            0
        }
        WM_APP_UPDATE_STATUS => {
            update_status(state);
            0
        }
        WM_DESTROY => {
            if !state.find_dialog.is_null() {
                unsafe { DestroyWindow(state.find_dialog) };
                state.find_dialog = null_mut();
            }
            unsafe { PostQuitMessage(0) };
            0
        }
        WM_NCDESTROY => {
            if !state.owned_font.is_null() {
                unsafe { DeleteObject(state.owned_font) };
                state.owned_font = null_mut();
            }
            unsafe {
                SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
                drop(Box::from_raw(state as *mut AppState));
                DefWindowProcW(hwnd, message, wparam, lparam)
            }
        }
        _ => unsafe { DefWindowProcW(hwnd, message, wparam, lparam) },
    }
}

unsafe fn state_from_hwnd(hwnd: HWND) -> Option<&'static mut AppState> {
    let pointer = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as *mut AppState;
    unsafe { pointer.as_mut() }
}

fn create_children(state: &mut AppState) -> Result<(), String> {
    state.editor = create_editor(state)?;
    state.status = unsafe {
        CreateWindowExW(
            0,
            STATUSCLASSNAMEW,
            null(),
            WS_CHILD | WS_VISIBLE,
            0,
            0,
            0,
            0,
            state.hwnd,
            ID_STATUS as *mut c_void,
            state.instance,
            null_mut(),
        )
    };
    if state.status.is_null() {
        return Err(dialogs::os_error("Unable to create the status bar"));
    }
    unsafe {
        SendMessageW(state.editor, EM_SETLIMITTEXT, 0x7FFF_FFFE, 0);
        let default_font = create_default_editor_font(state.hwnd);
        let font = if default_font.is_null() {
            GetStockObject(DEFAULT_GUI_FONT) as HFONT
        } else {
            state.owned_font = default_font;
            default_font
        };
        SendMessageW(state.editor, WM_SETFONT, font as usize, 1);
    }
    update_menu_state(state);
    update_status(state);
    Ok(())
}

fn create_editor(state: &AppState) -> Result<HWND, String> {
    let mut style = WS_CHILD
        | WS_VISIBLE
        | WS_VSCROLL
        | ES_LEFT as u32
        | ES_MULTILINE as u32
        | ES_AUTOVSCROLL as u32
        | ES_NOHIDESEL as u32
        | ES_WANTRETURN as u32;
    if !state.word_wrap {
        style |= WS_HSCROLL | ES_AUTOHSCROLL as u32;
    }
    let editor = unsafe {
        CreateWindowExW(
            WS_EX_CLIENTEDGE,
            dialogs::to_wide("EDIT").as_ptr(),
            null(),
            style,
            0,
            0,
            0,
            0,
            state.hwnd,
            ID_EDITOR as *mut c_void,
            state.instance,
            null_mut(),
        )
    };
    if editor.is_null() {
        Err(dialogs::os_error("Unable to create the editor"))
    } else {
        Ok(editor)
    }
}

fn create_default_editor_font(hwnd: HWND) -> HFONT {
    let dpi = unsafe { GetDpiForWindow(hwnd) }.max(96);
    let mut logical: LOGFONTW = unsafe { zeroed() };
    logical.lfHeight = -((10 * dpi as i32 + 36) / 72);
    logical.lfWeight = FW_NORMAL as i32;
    logical.lfCharSet = DEFAULT_CHARSET;
    logical.lfQuality = CLEARTYPE_QUALITY;
    logical.lfPitchAndFamily = FIXED_PITCH | FF_MODERN;
    let face = dialogs::to_wide("Lucida Console");
    let count = face.len().min(logical.lfFaceName.len());
    logical.lfFaceName[..count].copy_from_slice(&face[..count]);
    unsafe { CreateFontIndirectW(&logical) }
}

fn layout_children(state: &AppState) {
    let mut client: RECT = unsafe { zeroed() };
    unsafe { GetClientRect(state.hwnd, &mut client) };
    let width = client.right - client.left;
    let height = client.bottom - client.top;
    let status_height = if state.status_is_visible() {
        unsafe {
            ShowWindow(state.status, SW_SHOW);
            SendMessageW(state.status, WM_SIZE, 0, 0);
        }
        let mut status_rect: RECT = unsafe { zeroed() };
        unsafe { GetWindowRect(state.status, &mut status_rect) };
        status_rect.bottom - status_rect.top
    } else {
        unsafe { ShowWindow(state.status, SW_HIDE) };
        0
    };
    unsafe {
        MoveWindow(
            state.editor,
            0,
            0,
            width,
            (height - status_height).max(0),
            1,
        );
    }
}

fn create_menu() -> Result<HMENU, String> {
    unsafe {
        let bar = CreateMenu();
        let file = CreatePopupMenu();
        let edit = CreatePopupMenu();
        let format = CreatePopupMenu();
        let view = CreatePopupMenu();
        if bar.is_null() || file.is_null() || edit.is_null() || format.is_null() || view.is_null() {
            return Err(dialogs::os_error("Unable to create the menu"));
        }
        append(file, ID_FILE_NEW, "&New\tCtrl+N");
        append(file, ID_FILE_OPEN, "&Open...\tCtrl+O");
        append(file, ID_FILE_SAVE, "&Save\tCtrl+S");
        append(file, ID_FILE_SAVE_AS, "Save &As...\tCtrl+Shift+S");
        AppendMenuW(file, MF_SEPARATOR, 0, null());
        append(file, ID_FILE_EXIT, "E&xit");

        append(edit, ID_EDIT_UNDO, "&Undo\tCtrl+Z");
        AppendMenuW(edit, MF_SEPARATOR, 0, null());
        append(edit, ID_EDIT_CUT, "Cu&t\tCtrl+X");
        append(edit, ID_EDIT_COPY, "&Copy\tCtrl+C");
        append(edit, ID_EDIT_PASTE, "&Paste\tCtrl+V");
        append(edit, ID_EDIT_DELETE, "De&lete\tDel");
        AppendMenuW(edit, MF_SEPARATOR, 0, null());
        append(edit, ID_EDIT_FIND, "&Find...\tCtrl+F");
        append(edit, ID_EDIT_FIND_NEXT, "Find &Next\tF3");
        append(edit, ID_EDIT_FIND_PREVIOUS, "Find Pre&vious\tShift+F3");
        append(edit, ID_EDIT_REPLACE, "&Replace...\tCtrl+H");
        append(edit, ID_EDIT_GOTO, "&Go To...\tCtrl+G");
        AppendMenuW(edit, MF_SEPARATOR, 0, null());
        append(edit, ID_EDIT_SELECT_ALL, "Select &All\tCtrl+A");
        append(edit, ID_EDIT_TIME_DATE, "Time/&Date\tF5");

        append(format, ID_FORMAT_WRAP, "&Word Wrap");
        append(format, ID_FORMAT_FONT, "&Font...");
        append(view, ID_VIEW_STATUS, "&Status Bar");

        append_popup(bar, file, "&File");
        append_popup(bar, edit, "&Edit");
        append_popup(bar, format, "F&ormat");
        append_popup(bar, view, "&View");
        Ok(bar)
    }
}

unsafe fn append(menu: HMENU, id: usize, label: &str) {
    let wide = dialogs::to_wide(label);
    unsafe { AppendMenuW(menu, MF_STRING, id, wide.as_ptr()) };
}

unsafe fn append_popup(menu: HMENU, popup: HMENU, label: &str) {
    let wide = dialogs::to_wide(label);
    unsafe { AppendMenuW(menu, MF_POPUP, popup as usize, wide.as_ptr()) };
}

fn create_accelerators() -> Result<HACCEL, String> {
    const V: u8 = FVIRTKEY;
    const C: u8 = FVIRTKEY | FCONTROL;
    const S: u8 = FVIRTKEY | FSHIFT;
    const CS: u8 = FVIRTKEY | FCONTROL | FSHIFT;
    let entries = [
        accel(C, b'N', ID_FILE_NEW),
        accel(C, b'O', ID_FILE_OPEN),
        accel(C, b'S', ID_FILE_SAVE),
        accel(CS, b'S', ID_FILE_SAVE_AS),
        accel(C, b'Z', ID_EDIT_UNDO),
        accel(C, b'X', ID_EDIT_CUT),
        accel(C, b'C', ID_EDIT_COPY),
        accel(C, b'V', ID_EDIT_PASTE),
        accel(V, VK_DELETE as u8, ID_EDIT_DELETE),
        accel(C, b'F', ID_EDIT_FIND),
        accel(V, VK_F3 as u8, ID_EDIT_FIND_NEXT),
        accel(S, VK_F3 as u8, ID_EDIT_FIND_PREVIOUS),
        accel(C, b'H', ID_EDIT_REPLACE),
        accel(C, b'G', ID_EDIT_GOTO),
        accel(C, b'A', ID_EDIT_SELECT_ALL),
        accel(V, VK_F5 as u8, ID_EDIT_TIME_DATE),
    ];
    let handle = unsafe { CreateAcceleratorTableW(entries.as_ptr(), entries.len() as i32) };
    if handle.is_null() {
        Err(dialogs::os_error("Unable to create keyboard accelerators"))
    } else {
        Ok(handle)
    }
}

const fn accel(modifiers: u8, key: u8, command: usize) -> ACCEL {
    ACCEL {
        fVirt: modifiers,
        key: key as u16,
        cmd: command as u16,
    }
}

fn handle_command(state: &mut AppState, id: usize) {
    match id {
        ID_FILE_NEW => new_document(state),
        ID_FILE_OPEN => open_document(state),
        ID_FILE_SAVE => {
            save_document(state, false);
        }
        ID_FILE_SAVE_AS => {
            save_document(state, true);
        }
        ID_FILE_EXIT => unsafe {
            SendMessageW(state.hwnd, WM_CLOSE, 0, 0);
        },
        ID_EDIT_UNDO => unsafe {
            SendMessageW(state.editor, WM_UNDO, 0, 0);
        },
        ID_EDIT_CUT => unsafe {
            SendMessageW(state.editor, WM_CUT, 0, 0);
        },
        ID_EDIT_COPY => unsafe {
            SendMessageW(state.editor, WM_COPY, 0, 0);
        },
        ID_EDIT_PASTE => unsafe {
            SendMessageW(state.editor, WM_PASTE, 0, 0);
        },
        ID_EDIT_DELETE => unsafe {
            SendMessageW(state.editor, WM_CLEAR, 0, 0);
        },
        ID_EDIT_FIND => show_find_dialog(state, false),
        ID_EDIT_FIND_NEXT => find_next(state),
        ID_EDIT_FIND_PREVIOUS => find_previous(state),
        ID_EDIT_REPLACE => show_find_dialog(state, true),
        ID_EDIT_GOTO => go_to_line(state),
        ID_EDIT_SELECT_ALL => {
            unsafe { SendMessageW(state.editor, EM_SETSEL, 0, -1) };
            update_status(state);
        }
        ID_EDIT_TIME_DATE => insert_time_date(state),
        ID_FORMAT_WRAP => toggle_word_wrap(state),
        ID_FORMAT_FONT => choose_font(state),
        ID_VIEW_STATUS => toggle_status(state),
        _ => {}
    }
}

fn new_document(state: &mut AppState) {
    if !maybe_save(state) {
        return;
    }
    state.path = None;
    state.format = TextFormat::default();
    set_editor_text(state, "");
    state.dirty = false;
    state.set_title();
}

fn open_document(state: &mut AppState) {
    if !maybe_save(state) {
        return;
    }
    match dialogs::open_file(state.hwnd) {
        Ok(Some(path)) => open_path(state, path),
        Ok(None) => {}
        Err(message) => dialogs::show_error(Some(state.hwnd), APP_NAME, &message),
    }
}

fn open_command_line_path(state: &mut AppState, path: PathBuf) {
    if path.is_file() {
        open_path(state, path);
    } else if !path.exists() {
        if dialogs::confirm_create(state.hwnd, &path.to_string_lossy()) {
            state.path = Some(path);
            state.format = TextFormat::default();
            state.dirty = false;
            set_editor_text(state, "");
            state.set_title();
        }
    } else {
        dialogs::show_error(
            Some(state.hwnd),
            APP_NAME,
            &format!("The command-line path is not a file:\n\n{}", path.display()),
        );
    }
}

fn open_path(state: &mut AppState, path: PathBuf) {
    match file::load(&path) {
        Ok(loaded) => {
            let mut text = loaded.text;
            let appended_log_entry = is_log_document(&text)
                && current_time_date()
                    .is_some_and(|timestamp| append_log_entry(&mut text, &timestamp));
            set_editor_text(state, &text);
            state.path = Some(path);
            state.format = loaded.format;
            state.dirty = appended_log_entry;
            if appended_log_entry {
                let end = text.encode_utf16().count();
                unsafe {
                    SendMessageW(state.editor, EM_SETMODIFY, 1, 0);
                    SendMessageW(state.editor, EM_SETSEL, end, end as isize);
                    SendMessageW(state.editor, EM_SCROLLCARET, 0, 0);
                }
            }
            state.set_title();
            update_status(state);
        }
        Err(error) => dialogs::show_error(
            Some(state.hwnd),
            APP_NAME,
            &format!("Could not open the file:\n\n{error}"),
        ),
    }
}

fn save_document(state: &mut AppState, force_dialog: bool) -> bool {
    let path = if force_dialog || state.path.is_none() {
        match dialogs::save_file(state.hwnd, state.path.as_deref()) {
            Ok(Some(path)) => path,
            Ok(None) => return false,
            Err(message) => {
                dialogs::show_error(Some(state.hwnd), APP_NAME, &message);
                return false;
            }
        }
    } else {
        state.path.clone().unwrap()
    };
    let text = get_editor_text(state.editor);
    match file::save(&path, &text, state.format) {
        Ok(()) => {
            state.path = Some(path);
            state.dirty = false;
            state.set_title();
            true
        }
        Err(error) => {
            dialogs::show_error(
                Some(state.hwnd),
                APP_NAME,
                &format!("Could not save the file:\n\n{error}"),
            );
            false
        }
    }
}

fn maybe_save(state: &mut AppState) -> bool {
    if !state.dirty {
        return true;
    }
    match dialogs::confirm_save(state.hwnd, &state.display_name()) {
        SaveDecision::Save => save_document(state, false),
        SaveDecision::Discard => true,
        SaveDecision::Cancel => false,
    }
}

fn set_editor_text(state: &mut AppState, text: &str) {
    let wide = dialogs::to_wide(text);
    state.suppress_change = true;
    unsafe {
        SetWindowTextW(state.editor, wide.as_ptr());
        SendMessageW(state.editor, EM_SETMODIFY, 0, 0);
        SendMessageW(state.editor, EM_SETSEL, 0, 0);
    }
    state.suppress_change = false;
    update_status(state);
}

fn get_editor_text(editor: HWND) -> String {
    let length = unsafe { GetWindowTextLengthW(editor) };
    if length <= 0 {
        return String::new();
    }
    let mut buffer = vec![0u16; length as usize + 1];
    let written = unsafe { GetWindowTextW(editor, buffer.as_mut_ptr(), buffer.len() as i32) };
    String::from_utf16_lossy(&buffer[..written.max(0) as usize])
}

fn selection(editor: HWND) -> (u32, u32) {
    let mut start = 0u32;
    let mut end = 0u32;
    unsafe {
        SendMessageW(
            editor,
            EM_GETSEL,
            (&mut start as *mut u32) as usize,
            (&mut end as *mut u32) as isize,
        );
    }
    (start, end)
}

fn handle_drop(state: &mut AppState, drop: HDROP) {
    let length = unsafe { DragQueryFileW(drop, 0, null_mut(), 0) };
    if length > 0 {
        let mut buffer = vec![0u16; length as usize + 1];
        unsafe { DragQueryFileW(drop, 0, buffer.as_mut_ptr(), buffer.len() as u32) };
        let path = PathBuf::from(String::from_utf16_lossy(&buffer[..length as usize]));
        if maybe_save(state) {
            open_path(state, path);
        }
    }
    unsafe { DragFinish(drop) };
}

fn toggle_word_wrap(state: &mut AppState) {
    let text = get_editor_text(state.editor);
    let selected = selection(state.editor);
    let font = unsafe { SendMessageW(state.editor, WM_GETFONT, 0, 0) } as HFONT;
    state.word_wrap = !state.word_wrap;
    state.suppress_change = true;
    let old_editor = state.editor;
    match create_editor(state) {
        Ok(new_editor) => {
            state.editor = new_editor;
            let wide = dialogs::to_wide(&text);
            unsafe {
                SendMessageW(new_editor, EM_SETLIMITTEXT, 0x7FFF_FFFE, 0);
                SendMessageW(new_editor, WM_SETFONT, font as usize, 1);
                SetWindowTextW(new_editor, wide.as_ptr());
                SendMessageW(
                    new_editor,
                    EM_SETSEL,
                    selected.0 as usize,
                    selected.1 as isize,
                );
                SendMessageW(new_editor, EM_SETMODIFY, state.dirty as usize, 0);
                DestroyWindow(old_editor);
                SetFocus(new_editor);
            }
        }
        Err(message) => {
            state.word_wrap = !state.word_wrap;
            dialogs::show_error(Some(state.hwnd), APP_NAME, &message);
        }
    }
    state.suppress_change = false;
    update_menu_state(state);
    layout_children(state);
    update_status(state);
}

fn toggle_status(state: &mut AppState) {
    if state.word_wrap {
        return;
    }
    state.status_requested = !state.status_requested;
    update_menu_state(state);
    layout_children(state);
}

fn update_menu_state(state: &AppState) {
    unsafe {
        CheckMenuItem(
            state.menu,
            ID_FORMAT_WRAP as u32,
            MF_BYCOMMAND
                | if state.word_wrap {
                    MF_CHECKED
                } else {
                    MF_UNCHECKED
                },
        );
        CheckMenuItem(
            state.menu,
            ID_VIEW_STATUS as u32,
            MF_BYCOMMAND
                | if state.status_requested && !state.word_wrap {
                    MF_CHECKED
                } else {
                    MF_UNCHECKED
                },
        );
        EnableMenuItem(
            state.menu,
            ID_VIEW_STATUS as u32,
            MF_BYCOMMAND
                | if state.word_wrap {
                    MF_GRAYED
                } else {
                    MF_ENABLED
                },
        );
        EnableMenuItem(
            state.menu,
            ID_EDIT_GOTO as u32,
            MF_BYCOMMAND
                | if state.word_wrap {
                    MF_GRAYED
                } else {
                    MF_ENABLED
                },
        );
        DrawMenuBar(state.hwnd);
    }
}

fn update_status(state: &AppState) {
    if !state.status_is_visible() || state.editor.is_null() || state.status.is_null() {
        return;
    }
    let (caret, _) = selection(state.editor);
    let line = unsafe { SendMessageW(state.editor, EM_LINEFROMCHAR, caret as usize, 0) } as i32;
    let line_start = unsafe { SendMessageW(state.editor, EM_LINEINDEX, line as usize, 0) } as i32;
    let column = caret as i32 - line_start.max(0);
    let text = dialogs::to_wide(&format!("Ln {}, Col {}", line + 1, column + 1));
    unsafe { SendMessageW(state.status, SB_SETTEXTW, 0, text.as_ptr() as isize) };
}

fn choose_font(state: &mut AppState) {
    let current_font = unsafe { SendMessageW(state.editor, WM_GETFONT, 0, 0) } as HFONT;
    let mut logical: LOGFONTW = unsafe { zeroed() };
    if !current_font.is_null() {
        unsafe {
            GetObjectW(
                current_font,
                size_of::<LOGFONTW>() as i32,
                (&mut logical as *mut LOGFONTW).cast(),
            );
        }
    }
    if !dialogs::choose_font(state.hwnd, &mut logical) {
        return;
    }
    let font = unsafe { CreateFontIndirectW(&logical) };
    if font.is_null() {
        dialogs::show_error(
            Some(state.hwnd),
            APP_NAME,
            &dialogs::os_error("Unable to create the font"),
        );
        return;
    }
    unsafe {
        SendMessageW(state.editor, WM_SETFONT, font as usize, 1);
        if !state.owned_font.is_null() {
            DeleteObject(state.owned_font);
        }
    }
    state.owned_font = font;
}

fn insert_time_date(state: &AppState) {
    if let Some(value) = current_time_date() {
        replace_selection(state.editor, &value);
    }
}

fn current_time_date() -> Option<String> {
    let mut system_time: SYSTEMTIME = unsafe { zeroed() };
    unsafe { GetLocalTime(&mut system_time) };
    let mut time = [0u16; 128];
    let mut date = [0u16; 128];
    let time_len = unsafe {
        GetTimeFormatEx(
            null(),
            0,
            &system_time,
            null(),
            time.as_mut_ptr(),
            time.len() as i32,
        )
    };
    let date_len = unsafe {
        GetDateFormatEx(
            null(),
            0,
            &system_time,
            null(),
            date.as_mut_ptr(),
            date.len() as i32,
            null(),
        )
    };
    if time_len > 0 && date_len > 0 {
        Some(format!(
            "{} {}",
            String::from_utf16_lossy(&time[..time_len as usize - 1]),
            String::from_utf16_lossy(&date[..date_len as usize - 1])
        ))
    } else {
        None
    }
}

fn append_log_entry(text: &mut String, timestamp: &str) -> bool {
    if !is_log_document(text) {
        return false;
    }
    if !text.ends_with('\r') && !text.ends_with('\n') {
        text.push_str("\r\n");
    }
    text.push_str(timestamp);
    text.push_str("\r\n");
    true
}

fn is_log_document(text: &str) -> bool {
    text.strip_prefix(".LOG")
        .is_some_and(|rest| rest.is_empty() || rest.starts_with('\r') || rest.starts_with('\n'))
}

fn replace_selection(editor: HWND, text: &str) {
    let wide = dialogs::to_wide(text);
    unsafe { SendMessageW(editor, EM_REPLACESEL, 1, wide.as_ptr() as isize) };
}

fn go_to_line(state: &AppState) {
    if state.word_wrap {
        return;
    }
    let (caret, _) = selection(state.editor);
    let current =
        unsafe { SendMessageW(state.editor, EM_LINEFROMCHAR, caret as usize, 0) } as u32 + 1;
    if let Some(line) = dialogs::go_to_line(state.hwnd, state.instance, current) {
        let line_count = unsafe { SendMessageW(state.editor, EM_GETLINECOUNT, 0, 0) } as u32;
        if line == 0 || line > line_count {
            dialogs::show_error(
                Some(state.hwnd),
                "Go To Line",
                &format!("The line number must be between 1 and {line_count}."),
            );
            return;
        }
        let index = unsafe { SendMessageW(state.editor, EM_LINEINDEX, (line - 1) as usize, 0) };
        unsafe {
            SendMessageW(state.editor, EM_SETSEL, index as usize, index);
            SendMessageW(state.editor, EM_SCROLLCARET, 0, 0);
            SetFocus(state.editor);
        }
        update_status(state);
    }
}

fn show_find_dialog(state: &mut AppState, replace: bool) {
    if !state.find_dialog.is_null() {
        unsafe { SetForegroundWindow(state.find_dialog) };
        return;
    }
    if state.find_text[0] == 0 {
        let (start, end) = selection(state.editor);
        if end > start && end - start < 256 {
            let text: Vec<u16> = get_editor_text(state.editor).encode_utf16().collect();
            let selected = &text[start as usize..end as usize];
            state.find_text[..selected.len()].copy_from_slice(selected);
            state.find_text[selected.len()] = 0;
        }
    }
    let mut data = Box::<FINDREPLACEW>::default();
    data.lStructSize = size_of::<FINDREPLACEW>() as u32;
    data.hwndOwner = state.hwnd;
    data.Flags = state.find_flags;
    data.lpstrFindWhat = state.find_text.as_mut_ptr();
    data.wFindWhatLen = state.find_text.len() as u16;
    data.lpstrReplaceWith = state.replace_text.as_mut_ptr();
    data.wReplaceWithLen = state.replace_text.len() as u16;
    state.find_data = Some(data);
    let pointer = state.find_data.as_mut().unwrap().as_mut() as *mut FINDREPLACEW;
    state.find_dialog = unsafe {
        if replace {
            ReplaceTextW(pointer)
        } else {
            FindTextW(pointer)
        }
    };
    if state.find_dialog.is_null() {
        state.find_data = None;
        dialogs::show_error(
            Some(state.hwnd),
            APP_NAME,
            "Unable to open the Find dialog.",
        );
    }
}

fn handle_find_message(state: &mut AppState, data: *const FINDREPLACEW) {
    if data.is_null() {
        return;
    }
    let flags = unsafe { (*data).Flags };
    if flags & FR_DIALOGTERM != 0 {
        state.find_dialog = null_mut();
        state.find_data = None;
    } else {
        state.find_flags = flags & FIND_OPTION_FLAGS;
        if flags & FR_FINDNEXT != 0 {
            find_next_with_flags(state, flags);
        } else if flags & FR_REPLACE != 0 {
            replace_one(state, flags);
        } else if flags & FR_REPLACEALL != 0 {
            replace_all(state, flags);
        }
    }
}

fn find_next(state: &mut AppState) {
    state.find_flags |= FR_DOWN;
    if state.find_text[0] == 0 {
        show_find_dialog(state, false);
        return;
    }
    let flags = state.find_flags | FR_DOWN;
    find_next_with_flags(state, flags);
}

fn find_previous(state: &mut AppState) {
    state.find_flags &= !FR_DOWN;
    if state.find_text[0] == 0 {
        show_find_dialog(state, false);
        return;
    }
    let flags = state.find_flags & !FR_DOWN;
    find_next_with_flags(state, flags);
}

fn find_next_with_flags(state: &AppState, flags: u32) -> bool {
    let needle = nul_terminated_slice(state.find_text.as_ref());
    if needle.is_empty() {
        return false;
    }
    let haystack: Vec<u16> = get_editor_text(state.editor).encode_utf16().collect();
    let (start, end) = selection(state.editor);
    let found = if flags & FR_DOWN != 0 {
        find_utf16(&haystack, needle, end as usize, true, flags)
    } else {
        find_utf16(&haystack, needle, start as usize, false, flags)
    };
    if let Some(index) = found {
        unsafe {
            SendMessageW(
                state.editor,
                EM_SETSEL,
                index,
                (index + needle.len()) as isize,
            );
            SendMessageW(state.editor, EM_SCROLLCARET, 0, 0);
            SetFocus(state.editor);
        }
        update_status(state);
        true
    } else {
        let find = String::from_utf16_lossy(needle);
        let message = dialogs::to_wide(&format!("Cannot find \"{find}\""));
        let title = dialogs::to_wide(APP_NAME);
        unsafe {
            MessageBoxW(
                state.hwnd,
                message.as_ptr(),
                title.as_ptr(),
                MB_OK | MB_ICONINFORMATION,
            )
        };
        false
    }
}

fn replace_one(state: &AppState, flags: u32) {
    let needle = nul_terminated_slice(state.find_text.as_ref());
    let replacement = String::from_utf16_lossy(nul_terminated_slice(state.replace_text.as_ref()));
    let (start, end) = selection(state.editor);
    let text: Vec<u16> = get_editor_text(state.editor).encode_utf16().collect();
    if selection_matches(&text, start as usize, end as usize, needle, flags) {
        replace_selection(state.editor, &replacement);
    }
    find_next_with_flags(state, flags);
}

fn selection_matches(text: &[u16], start: usize, end: usize, needle: &[u16], flags: u32) -> bool {
    end > start
        && end <= text.len()
        && slices_equal(&text[start..end], needle, flags)
        && (flags & FR_WHOLEWORD == 0 || whole_word_at(text, start, needle.len()))
}

fn replace_all(state: &mut AppState, flags: u32) {
    let needle = nul_terminated_slice(state.find_text.as_ref()).to_vec();
    let replacement = nul_terminated_slice(state.replace_text.as_ref()).to_vec();
    if needle.is_empty() {
        return;
    }
    let input: Vec<u16> = get_editor_text(state.editor).encode_utf16().collect();
    let (output, count) = replace_all_utf16(&input, &needle, &replacement, flags);
    if count > 0 {
        set_editor_text(state, &String::from_utf16_lossy(&output));
        state.dirty = true;
        unsafe { SendMessageW(state.editor, EM_SETMODIFY, 1, 0) };
        state.set_title();
    }
    let message = dialogs::to_wide(&format!("Replaced {count} occurrence(s)."));
    let title = dialogs::to_wide(APP_NAME);
    unsafe {
        MessageBoxW(
            state.hwnd,
            message.as_ptr(),
            title.as_ptr(),
            MB_OK | MB_ICONINFORMATION,
        )
    };
}

fn replace_all_utf16(
    input: &[u16],
    needle: &[u16],
    replacement: &[u16],
    flags: u32,
) -> (Vec<u16>, usize) {
    if needle.is_empty() {
        return (input.to_vec(), 0);
    }
    let mut output = Vec::with_capacity(input.len());
    let mut index = 0;
    let mut count = 0;
    while index < input.len() {
        let matches = index + needle.len() <= input.len()
            && slices_equal(&input[index..index + needle.len()], needle, flags)
            && (!(flags & FR_WHOLEWORD != 0) || whole_word_at(input, index, needle.len()));
        if matches {
            output.extend_from_slice(replacement);
            index += needle.len();
            count += 1;
        } else {
            output.push(input[index]);
            index += 1;
        }
    }
    (output, count)
}

fn find_utf16(
    haystack: &[u16],
    needle: &[u16],
    from: usize,
    down: bool,
    flags: u32,
) -> Option<usize> {
    if needle.is_empty() || needle.len() > haystack.len() {
        return None;
    }
    let max = haystack.len() - needle.len();
    if down {
        let first = from.min(haystack.len());
        if first > max {
            return None;
        }
        (first..=max).find(|&index| {
            slices_equal(&haystack[index..index + needle.len()], needle, flags)
                && (flags & FR_WHOLEWORD == 0 || whole_word_at(haystack, index, needle.len()))
        })
    } else {
        let last = from.saturating_sub(needle.len()).min(max);
        (0..=last).rev().find(|&index| {
            index + needle.len() <= from
                && slices_equal(&haystack[index..index + needle.len()], needle, flags)
                && (flags & FR_WHOLEWORD == 0 || whole_word_at(haystack, index, needle.len()))
        })
    }
}

fn slices_equal(left: &[u16], right: &[u16], flags: u32) -> bool {
    if left.len() != right.len() {
        return false;
    }
    if flags & FR_MATCHCASE != 0 {
        left == right
    } else {
        unsafe {
            CompareStringOrdinal(
                left.as_ptr(),
                left.len() as i32,
                right.as_ptr(),
                right.len() as i32,
                1,
            ) == CSTR_EQUAL
        }
    }
}

fn whole_word_at(text: &[u16], start: usize, length: usize) -> bool {
    let before = start
        .checked_sub(1)
        .and_then(|index| text.get(index))
        .copied();
    let after = text.get(start + length).copied();
    !before.is_some_and(is_word_unit) && !after.is_some_and(is_word_unit)
}

fn is_word_unit(unit: u16) -> bool {
    char::from_u32(unit as u32).is_some_and(|ch| ch.is_alphanumeric() || ch == '_')
}

fn nul_terminated_slice(buffer: &[u16]) -> &[u16] {
    let len = buffer
        .iter()
        .position(|&unit| unit == 0)
        .unwrap_or(buffer.len());
    &buffer[..len]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_forward_and_backward_in_utf16_offsets() {
        let text: Vec<u16> = "one two one".encode_utf16().collect();
        let needle: Vec<u16> = "one".encode_utf16().collect();
        assert_eq!(find_utf16(&text, &needle, 0, true, FR_DOWN), Some(0));
        assert_eq!(find_utf16(&text, &needle, 4, true, FR_DOWN), Some(8));
        assert_eq!(find_utf16(&text, &needle, text.len(), false, 0), Some(8));
    }

    #[test]
    fn case_and_whole_word_options_are_respected() {
        let text: Vec<u16> = "Cat category cat".encode_utf16().collect();
        let needle: Vec<u16> = "cat".encode_utf16().collect();
        assert_eq!(
            find_utf16(&text, &needle, 0, true, FR_DOWN | FR_WHOLEWORD),
            Some(0)
        );
        assert_eq!(
            find_utf16(
                &text,
                &needle,
                0,
                true,
                FR_DOWN | FR_WHOLEWORD | FR_MATCHCASE,
            ),
            Some(13)
        );
    }

    #[test]
    fn whole_word_replacement_rejects_a_partial_selection() {
        let text: Vec<u16> = "category cat".encode_utf16().collect();
        let needle: Vec<u16> = "cat".encode_utf16().collect();
        assert!(!selection_matches(&text, 0, 3, &needle, FR_WHOLEWORD));
        assert!(selection_matches(&text, 9, 12, &needle, FR_WHOLEWORD));
    }

    #[test]
    fn log_entry_is_appended_only_to_dot_log_documents() {
        let mut log = ".LOG\r\nfirst entry".to_owned();
        assert!(append_log_entry(&mut log, "10:30 AM 8/26/2026"));
        assert_eq!(log, ".LOG\r\nfirst entry\r\n10:30 AM 8/26/2026\r\n");

        let mut ordinary = "ordinary text".to_owned();
        assert!(!append_log_entry(&mut ordinary, "ignored"));
        assert_eq!(ordinary, "ordinary text");

        let mut marker_only = ".LOG".to_owned();
        assert!(append_log_entry(&mut marker_only, "10:30 AM 8/26/2026"));
        assert_eq!(marker_only, ".LOG\r\n10:30 AM 8/26/2026\r\n");

        assert!(is_log_document(".LOG\n"));
        assert!(is_log_document(".LOG\r"));
        assert!(!is_log_document(".LOGGER"));
    }

    #[test]
    fn replace_all_uses_non_overlapping_matches() {
        let input: Vec<u16> = "ababa".encode_utf16().collect();
        let needle: Vec<u16> = "aba".encode_utf16().collect();
        let replacement: Vec<u16> = "X".encode_utf16().collect();
        let (output, count) = replace_all_utf16(&input, &needle, &replacement, FR_MATCHCASE);
        assert_eq!(String::from_utf16_lossy(&output), "Xba");
        assert_eq!(count, 1);
    }

    #[test]
    fn replace_all_respects_case_matching() {
        let input: Vec<u16> = "Cat cat".encode_utf16().collect();
        let needle: Vec<u16> = "cat".encode_utf16().collect();
        let replacement: Vec<u16> = "dog".encode_utf16().collect();

        let (insensitive, insensitive_count) = replace_all_utf16(&input, &needle, &replacement, 0);
        assert_eq!(String::from_utf16_lossy(&insensitive), "dog dog");
        assert_eq!(insensitive_count, 2);

        let (sensitive, sensitive_count) =
            replace_all_utf16(&input, &needle, &replacement, FR_MATCHCASE);
        assert_eq!(String::from_utf16_lossy(&sensitive), "Cat dog");
        assert_eq!(sensitive_count, 1);
    }

    #[test]
    fn replace_all_respects_whole_word_boundaries() {
        let input: Vec<u16> = "cat category cat_ cat".encode_utf16().collect();
        let needle: Vec<u16> = "cat".encode_utf16().collect();
        let replacement: Vec<u16> = "dog".encode_utf16().collect();
        let (output, count) = replace_all_utf16(&input, &needle, &replacement, FR_WHOLEWORD);
        assert_eq!(String::from_utf16_lossy(&output), "dog category cat_ dog");
        assert_eq!(count, 2);
    }

    #[test]
    fn word_boundaries_treat_digits_and_underscores_as_word_characters() {
        let text: Vec<u16> = "var var_name var2 (var) var-name".encode_utf16().collect();
        let needle: Vec<u16> = "var".encode_utf16().collect();
        let replacement: Vec<u16> = "x".encode_utf16().collect();
        let (output, count) = replace_all_utf16(&text, &needle, &replacement, FR_WHOLEWORD);
        assert_eq!(
            String::from_utf16_lossy(&output),
            "x var_name var2 (x) x-name"
        );
        assert_eq!(count, 3);
    }
}
