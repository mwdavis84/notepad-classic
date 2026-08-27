use std::cell::{Cell, RefCell, UnsafeCell};
use std::ffi::{OsStr, OsString, c_void};
use std::io;
use std::mem::{size_of, zeroed};
use std::os::windows::ffi::OsStringExt;
use std::path::{Path, PathBuf};
use std::ptr::{null, null_mut};
use std::rc::Rc;

use windows_sys::Win32::Foundation::{
    HINSTANCE, HWND, LPARAM, LRESULT, RECT, SIZE, SYSTEMTIME, WPARAM,
};
use windows_sys::Win32::Globalization::{
    CSTR_EQUAL, CompareStringOrdinal, GetDateFormatEx, GetTimeFormatEx,
};
use windows_sys::Win32::Graphics::Gdi::{
    CLEARTYPE_QUALITY, COLOR_WINDOW, CreateFontIndirectW, DEFAULT_CHARSET, DEFAULT_GUI_FONT,
    DeleteObject, FF_MODERN, FIXED_PITCH, FW_NORMAL, GetDC, GetStockObject, GetSysColorBrush,
    GetTextExtentPoint32W, HFONT, LOGFONTW, ReleaseDC, SelectObject, UpdateWindow,
};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::System::SystemInformation::GetLocalTime;
use windows_sys::Win32::System::SystemServices::{MK_CONTROL, MK_LBUTTON};
use windows_sys::Win32::UI::Controls::Dialogs::{
    FINDMSGSTRINGW, FINDREPLACEW, FR_DIALOGTERM, FR_DOWN, FR_FINDNEXT, FR_MATCHCASE, FR_REPLACE,
    FR_REPLACEALL, FR_WHOLEWORD, FindTextW, ReplaceTextW,
};
use windows_sys::Win32::UI::Controls::{
    EM_GETLINECOUNT, EM_GETSEL, EM_LINEFROMCHAR, EM_LINEINDEX, EM_REPLACESEL, EM_SCROLLCARET,
    EM_SETLIMITTEXT, EM_SETMODIFY, EM_SETSEL, ICC_BAR_CLASSES, ICC_LINK_CLASS,
    INITCOMMONCONTROLSEX, InitCommonControlsEx, SB_SETPARTS, SB_SETTEXTW, STATUSCLASSNAMEW,
};
use windows_sys::Win32::UI::HiDpi::{
    DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, GetDpiForWindow, SetProcessDpiAwarenessContext,
};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    SetFocus, VK_ADD, VK_DELETE, VK_F3, VK_F5, VK_OEM_MINUS, VK_OEM_PLUS, VK_SUBTRACT,
};
use windows_sys::Win32::UI::Shell::{DragAcceptFiles, DragFinish, DragQueryFileW, HDROP};
use windows_sys::Win32::UI::WindowsAndMessaging::*;

use crate::dialogs::{self, SaveDecision};
use crate::file::{self, TextFormat};
use crate::localization::ids::*;
use crate::localization::{self, FormatArg};
use crate::printing;
use crate::window_placement;

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

const WM_APP_UPDATE_STATUS: u32 = WM_APP + 1;
const FIND_OPTION_FLAGS: u32 = FR_DOWN | FR_MATCHCASE | FR_WHOLEWORD;
const ZOOM_LEVELS: [u16; 24] = [
    10, 20, 30, 40, 50, 60, 70, 80, 90, 100, 110, 120, 130, 140, 150, 160, 170, 180, 190, 200, 250,
    300, 400, 500,
];
const DEFAULT_ZOOM_INDEX: usize = 9;

fn app_name() -> String {
    localized_string(IDS_APP_NAME)
}

fn localized_string(id: usize) -> String {
    let value = localization::text(id);
    String::from_utf16_lossy(localization::without_trailing_nul(&value))
}

fn localized_error(id: usize, detail: impl std::fmt::Display) -> String {
    let detail = detail.to_string().encode_utf16().collect::<Vec<_>>();
    let text = localization::format(id, &[FormatArg::Wide(&detail)]);
    String::from_utf16_lossy(localization::without_trailing_nul(&text))
}

#[derive(Clone, Copy)]
pub(crate) struct FontChoice {
    pub(crate) logical: LOGFONTW,
    pub(crate) point_size_tenths: i32,
}

struct AppState {
    instance: HINSTANCE,
    hwnd: Cell<HWND>,
    editor: Cell<HWND>,
    status: Cell<HWND>,
    menu: HMENU,
    path: RefCell<Option<PathBuf>>,
    format: Cell<TextFormat>,
    dirty: Cell<bool>,
    suppress_change: Cell<bool>,
    word_wrap: Cell<bool>,
    status_requested: Cell<bool>,
    owned_font: Cell<HFONT>,
    dpi: Cell<u32>,
    font_choice: Cell<FontChoice>,
    zoom_index: Cell<usize>,
    wheel_delta_remainder: Cell<i32>,
    find_message: u32,
    find_dialog: Cell<HWND>,
    find_data: RefCell<Option<Box<FINDREPLACEW>>>,
    find_flags: Cell<u32>,
    find_text: UnsafeCell<[u16; 256]>,
    replace_text: UnsafeCell<[u16; 256]>,
}

impl AppState {
    fn new(instance: HINSTANCE, menu: HMENU, find_message: u32) -> Self {
        Self {
            instance,
            hwnd: Cell::new(null_mut()),
            editor: Cell::new(null_mut()),
            status: Cell::new(null_mut()),
            menu,
            path: RefCell::new(None),
            format: Cell::new(TextFormat::default()),
            dirty: Cell::new(false),
            suppress_change: Cell::new(false),
            word_wrap: Cell::new(false),
            status_requested: Cell::new(true),
            owned_font: Cell::new(null_mut()),
            dpi: Cell::new(96),
            font_choice: Cell::new(default_font_choice()),
            zoom_index: Cell::new(DEFAULT_ZOOM_INDEX),
            wheel_delta_remainder: Cell::new(0),
            find_message,
            find_dialog: Cell::new(null_mut()),
            find_data: RefCell::new(None),
            find_flags: Cell::new(FR_DOWN),
            find_text: UnsafeCell::new([0; 256]),
            replace_text: UnsafeCell::new([0; 256]),
        }
    }

    fn display_name(&self) -> OsString {
        self.path
            .borrow()
            .as_deref()
            .and_then(Path::file_name)
            .map(OsStr::to_os_string)
            .unwrap_or_else(|| {
                let text = localization::text(IDS_UNTITLED);
                OsString::from_wide(localization::without_trailing_nul(&text))
            })
    }

    fn set_title(&self) {
        let display_name = self.display_name();
        let mut title = Vec::new();
        if self.dirty.get() {
            title.push(b'*' as u16);
        }
        let localized = localization::format(IDS_WINDOW_TITLE, &[FormatArg::Os(&display_name)]);
        title.extend_from_slice(localization::without_trailing_nul(&localized));
        title.push(0);
        unsafe { SetWindowTextW(self.hwnd.get(), title.as_ptr()) };
    }

    fn status_is_visible(&self) -> bool {
        self.status_requested.get() && !self.word_wrap.get()
    }
}

pub fn run() -> Result<(), String> {
    unsafe {
        // Failure only means an older Windows DPI mode remains in effect.
        SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    }
    let controls = INITCOMMONCONTROLSEX {
        dwSize: size_of::<INITCOMMONCONTROLSEX>() as u32,
        dwICC: ICC_BAR_CLASSES | ICC_LINK_CLASS,
    };
    unsafe { InitCommonControlsEx(&controls) };

    let instance = unsafe { GetModuleHandleW(null()) };
    if instance.is_null() {
        // Capture immediately: localization and cleanup also call Win32 APIs and
        // must never replace the error reported by the failed operation.
        let error = io::Error::last_os_error();
        return Err(dialogs::os_error(
            &localized_string(IDS_GET_MODULE_FAILED),
            &error,
        ));
    }
    let find_message = unsafe { RegisterWindowMessageW(FINDMSGSTRINGW) };
    if find_message == 0 {
        let error = io::Error::last_os_error();
        return Err(dialogs::os_error(
            &localized_string(IDS_REGISTER_FIND_FAILED),
            &error,
        ));
    }

    let icon = unsafe { LoadIconW(instance, IDI_APP_ICON as *const u16) };
    if icon.is_null() {
        let error = io::Error::last_os_error();
        return Err(dialogs::os_error(
            &localized_string(IDS_LOAD_ICON_FAILED),
            &error,
        ));
    }
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
        let error = io::Error::last_os_error();
        return Err(dialogs::os_error(
            &localized_string(IDS_REGISTER_CLASS_FAILED),
            &error,
        ));
    }

    let menu = create_menu()?;
    let state = Rc::new(AppState::new(instance, menu, find_message));
    let untitled = localization::text(IDS_UNTITLED);
    let title = localization::format(
        IDS_WINDOW_TITLE,
        &[FormatArg::Wide(localization::without_trailing_nul(
            &untitled,
        ))],
    );
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
            Rc::as_ptr(&state).cast_mut().cast(),
        )
    };
    if hwnd.is_null() {
        let error = io::Error::last_os_error();
        return Err(dialogs::os_error(
            &localized_string(IDS_CREATE_WINDOW_FAILED),
            &error,
        ));
    }

    let show_cmd = match window_placement::load_window_placement() {
        Some(saved) => {
            if unsafe { window_placement::apply_window_placement(hwnd, &saved) } {
                if saved.is_maximized {
                    SW_SHOWMAXIMIZED
                } else {
                    SW_SHOWNORMAL
                }
            } else {
                SW_SHOW
            }
        }
        None => SW_SHOW,
    };

    unsafe {
        ShowWindow(hwnd, show_cmd);
        UpdateWindow(hwnd);
    }

    if let Some(argument) = std::env::args_os().nth(1) {
        let path = PathBuf::from(argument);
        open_command_line_path(&state, path);
    }

    let accelerator = create_accelerators()?;
    let mut message: MSG = unsafe { zeroed() };
    loop {
        let result = unsafe { GetMessageW(&mut message, null_mut(), 0, 0) };
        if result == -1 {
            let error = io::Error::last_os_error();
            unsafe { DestroyAcceleratorTable(accelerator) };
            return Err(dialogs::os_error(
                &localized_string(IDS_MESSAGE_LOOP_FAILED),
                &error,
            ));
        }
        if result == 0 {
            break;
        }

        let find_dialog = state.find_dialog.get();
        if !find_dialog.is_null() && unsafe { IsDialogMessageW(find_dialog, &message) } != 0 {
            continue;
        }
        if unsafe { TranslateAcceleratorW(hwnd, accelerator, &message) } != 0 {
            continue;
        }
        if message.message == WM_MOUSEWHEEL {
            if (message.wParam as u32 & 0xFFFF & MK_CONTROL) != 0
                && (message.hwnd == hwnd || unsafe { IsChild(hwnd, message.hwnd) } != 0)
            {
                let delta = ((message.wParam >> 16) as u16 as i16) as i32;
                let (steps, remainder) =
                    accumulate_wheel_delta(state.wheel_delta_remainder.get(), delta);
                state.wheel_delta_remainder.set(remainder);
                if steps != 0 {
                    change_zoom(&state, steps);
                }
                continue;
            }
            state.wheel_delta_remainder.set(0);
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
            (*state).hwnd.set(hwnd);
            // `run` owns one `Rc`; this increment creates the distinct strong
            // reference owned by the raw pointer in `GWLP_USERDATA`.
            Rc::increment_strong_count(state);
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, state as isize);
        }
        return 1;
    }

    let Some(state) = (unsafe { clone_state_from_hwnd(hwnd) }) else {
        return unsafe { DefWindowProcW(hwnd, message, wparam, lparam) };
    };

    if message == state.find_message {
        handle_find_message(&state, lparam as *const FINDREPLACEW);
        return 0;
    }

    match message {
        WM_CREATE => match create_children(&state) {
            Ok(()) => {
                unsafe { DragAcceptFiles(hwnd, 1) };
                state.set_title();
                0
            }
            Err(message) => {
                dialogs::show_error(Some(hwnd), &app_name(), &message);
                -1
            }
        },
        WM_SIZE => {
            layout_children(&state);
            0
        }
        WM_DPICHANGED => {
            handle_dpi_changed(&state, wparam, lparam);
            0
        }
        WM_SETFOCUS => {
            unsafe { SetFocus(state.editor.get()) };
            0
        }
        WM_COMMAND => {
            let id = wparam & 0xFFFF;
            let notification = (wparam >> 16) & 0xFFFF;
            if id == IDC_EDITOR && notification == EN_CHANGE as usize {
                if !state.suppress_change.get() {
                    state.dirty.set(true);
                    state.set_title();
                }
                update_status_position(&state);
            } else {
                handle_command(&state, id);
            }
            0
        }
        WM_DROPFILES => {
            handle_drop(&state, wparam as HDROP);
            0
        }
        WM_CLOSE => {
            if maybe_save(&state) {
                window_placement::save_window_placement(hwnd);
                unsafe { DestroyWindow(hwnd) };
            }
            0
        }
        WM_APP_UPDATE_STATUS => {
            update_status_position(&state);
            0
        }
        WM_DESTROY => {
            let find_dialog = state.find_dialog.replace(null_mut());
            if !find_dialog.is_null() {
                unsafe { DestroyWindow(find_dialog) };
            }
            unsafe { PostQuitMessage(0) };
            0
        }
        WM_NCDESTROY => {
            let stored = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as *const AppState;
            unsafe {
                // Clearing userdata prevents new lookups. This consumes exactly
                // the window-owned count created during `WM_NCCREATE`; `state`
                // and any outer handlers retain their temporary strong counts.
                SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
                if !stored.is_null() {
                    drop(Rc::from_raw(stored));
                }
            }
            state.hwnd.set(null_mut());
            let owned_font = state.owned_font.replace(null_mut());
            if !owned_font.is_null() {
                unsafe { DeleteObject(owned_font) };
            }
            unsafe { DefWindowProcW(hwnd, message, wparam, lparam) }
        }
        _ => unsafe { DefWindowProcW(hwnd, message, wparam, lparam) },
    }
}

unsafe fn clone_state_from_hwnd(hwnd: HWND) -> Option<Rc<AppState>> {
    let pointer = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as *const AppState;
    if pointer.is_null() {
        return None;
    }
    // The stored raw pointer owns a strong count. Increment before reconstructing
    // so this temporary `Rc` cannot consume the window-owned reference.
    unsafe {
        Rc::increment_strong_count(pointer);
        Some(Rc::from_raw(pointer))
    }
}

fn create_children(state: &AppState) -> Result<(), String> {
    let dpi = unsafe { GetDpiForWindow(state.hwnd.get()) }.max(96);
    state.dpi.set(dpi);
    let editor = create_editor(state)?;
    state.editor.set(editor);
    let status = unsafe {
        CreateWindowExW(
            0,
            STATUSCLASSNAMEW,
            null(),
            WS_CHILD | WS_VISIBLE,
            0,
            0,
            0,
            0,
            state.hwnd.get(),
            IDC_STATUS as *mut c_void,
            state.instance,
            null_mut(),
        )
    };
    if status.is_null() {
        let error = io::Error::last_os_error();
        return Err(dialogs::os_error(
            &localized_string(IDS_CREATE_STATUS_FAILED),
            &error,
        ));
    }
    state.status.set(status);
    unsafe {
        SendMessageW(editor, EM_SETLIMITTEXT, 0x7FFF_FFFE, 0);
        let default_font = create_editor_font(
            state.font_choice.get(),
            dpi,
            zoom_percent(state.zoom_index.get()),
        );
        let font = if default_font.is_null() {
            GetStockObject(DEFAULT_GUI_FONT) as HFONT
        } else {
            default_font
        };
        SendMessageW(editor, WM_SETFONT, font as usize, 1);
        if !default_font.is_null() {
            if state.hwnd.get().is_null() {
                DeleteObject(default_font);
            } else {
                state.owned_font.set(default_font);
            }
        }
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
    if !state.word_wrap.get() {
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
            state.hwnd.get(),
            IDC_EDITOR as *mut c_void,
            state.instance,
            null_mut(),
        )
    };
    if editor.is_null() {
        let error = io::Error::last_os_error();
        Err(dialogs::os_error(
            &localized_string(IDS_CREATE_EDITOR_FAILED),
            &error,
        ))
    } else {
        Ok(editor)
    }
}

fn default_font_choice() -> FontChoice {
    let mut logical: LOGFONTW = unsafe { zeroed() };
    logical.lfWeight = FW_NORMAL as i32;
    logical.lfCharSet = DEFAULT_CHARSET;
    logical.lfQuality = CLEARTYPE_QUALITY;
    logical.lfPitchAndFamily = FIXED_PITCH | FF_MODERN;
    let face = dialogs::to_wide("Lucida Console");
    let count = face.len().min(logical.lfFaceName.len());
    logical.lfFaceName[..count].copy_from_slice(&face[..count]);
    FontChoice {
        logical,
        point_size_tenths: 100,
    }
}

fn create_editor_font(choice: FontChoice, dpi: u32, zoom_percentage: u16) -> HFONT {
    let mut logical = choice.logical;
    logical.lfHeight = rendered_font_height(choice.point_size_tenths, dpi, zoom_percentage);
    logical.lfWidth = 0;
    unsafe { CreateFontIndirectW(&logical) }
}

pub(crate) fn rendered_font_height(point_size_tenths: i32, dpi: u32, zoom_percentage: u16) -> i32 {
    let numerator = i64::from(point_size_tenths.max(1))
        * i64::from(dpi.max(1))
        * i64::from(zoom_percentage.max(1));
    let pixels = ((numerator + 36_000) / 72_000).clamp(1, i64::from(i32::MAX));
    -(pixels as i32)
}

fn replace_editor_font(state: &AppState, dpi: u32) -> bool {
    replace_editor_font_for(
        state,
        state.font_choice.get(),
        dpi,
        zoom_percent(state.zoom_index.get()),
    )
}

fn replace_editor_font_for(
    state: &AppState,
    choice: FontChoice,
    dpi: u32,
    zoom_percentage: u16,
) -> bool {
    let font = create_editor_font(choice, dpi, zoom_percentage);
    if font.is_null() {
        return false;
    }
    let editor = state.editor.get();
    unsafe { SendMessageW(editor, WM_SETFONT, font as usize, 1) };
    if state.hwnd.get().is_null() {
        unsafe { DeleteObject(font) };
        return false;
    }
    let previous = state.owned_font.replace(font);
    if !previous.is_null() {
        unsafe { DeleteObject(previous) };
    }
    true
}

const fn zoom_percent(index: usize) -> u16 {
    ZOOM_LEVELS[if index < ZOOM_LEVELS.len() {
        index
    } else {
        DEFAULT_ZOOM_INDEX
    }]
}

fn stepped_zoom_index(index: usize, steps: i32) -> usize {
    if steps >= 0 {
        index
            .saturating_add(steps as usize)
            .min(ZOOM_LEVELS.len() - 1)
    } else {
        index.saturating_sub(steps.unsigned_abs() as usize)
    }
}

fn accumulate_wheel_delta(remainder: i32, delta: i32) -> (i32, i32) {
    let total = remainder.saturating_add(delta);
    (total / WHEEL_DELTA as i32, total % WHEEL_DELTA as i32)
}

fn change_zoom(state: &AppState, steps: i32) {
    let current = state.zoom_index.get();
    let next = stepped_zoom_index(current, steps);
    if next == current {
        return;
    }
    if !replace_editor_font_for(
        state,
        state.font_choice.get(),
        state.dpi.get(),
        zoom_percent(next),
    ) {
        let error = io::Error::last_os_error();
        dialogs::show_error(
            Some(state.hwnd.get()),
            &app_name(),
            &dialogs::os_error(&localized_string(IDS_CREATE_FONT_FAILED), &error),
        );
        return;
    }
    state.zoom_index.set(next);
    update_menu_state(state);
    update_status(state);
}

fn handle_dpi_changed(state: &AppState, wparam: WPARAM, lparam: LPARAM) {
    let suggested = unsafe { *(lparam as *const RECT) };
    let dpi = ((wparam >> 16) as u16 as u32).max(96);
    unsafe {
        SetWindowPos(
            state.hwnd.get(),
            null_mut(),
            suggested.left,
            suggested.top,
            suggested.right - suggested.left,
            suggested.bottom - suggested.top,
            SWP_NOZORDER | SWP_NOACTIVATE,
        );
    }
    state.dpi.set(dpi);
    replace_editor_font(state, dpi);
    layout_children(state);
}

fn layout_children(state: &AppState) {
    let hwnd = state.hwnd.get();
    let editor = state.editor.get();
    let status = state.status.get();
    let mut client: RECT = unsafe { zeroed() };
    unsafe { GetClientRect(hwnd, &mut client) };
    let width = client.right - client.left;
    let height = client.bottom - client.top;
    let status_height = if state.status_is_visible() {
        unsafe {
            ShowWindow(status, SW_SHOW);
            SendMessageW(status, WM_SIZE, 0, 0);
        }
        let mut status_rect: RECT = unsafe { zeroed() };
        unsafe { GetWindowRect(status, &mut status_rect) };
        status_rect.bottom - status_rect.top
    } else {
        unsafe { ShowWindow(status, SW_HIDE) };
        0
    };
    unsafe {
        MoveWindow(editor, 0, 0, width, (height - status_height).max(0), 1);
    }
    update_status(state);
}

fn create_menu() -> Result<HMENU, String> {
    match localization::menu(IDR_MAIN_MENU) {
        Some(menu) => Ok(menu),
        None => {
            let error = io::Error::last_os_error();
            Err(dialogs::os_error(
                &localized_string(IDS_LOAD_MENU_FAILED),
                &error,
            ))
        }
    }
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
        accel(C, b'P', ID_FILE_PRINT),
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
        accel(C, VK_OEM_PLUS as u8, ID_VIEW_ZOOM_IN),
        accel(CS, VK_OEM_PLUS as u8, ID_VIEW_ZOOM_IN),
        accel(C, VK_ADD as u8, ID_VIEW_ZOOM_IN),
        accel(C, VK_OEM_MINUS as u8, ID_VIEW_ZOOM_OUT),
        accel(C, VK_SUBTRACT as u8, ID_VIEW_ZOOM_OUT),
        accel(C, b'0', ID_VIEW_ZOOM_DEFAULT),
    ];
    let handle = unsafe { CreateAcceleratorTableW(entries.as_ptr(), entries.len() as i32) };
    if handle.is_null() {
        let error = io::Error::last_os_error();
        Err(dialogs::os_error(
            &localized_string(IDS_CREATE_ACCELERATORS_FAILED),
            &error,
        ))
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

fn handle_command(state: &AppState, id: usize) {
    match id {
        ID_FILE_NEW => new_document(state),
        ID_FILE_OPEN => open_document(state),
        ID_FILE_SAVE => {
            save_document(state, false);
        }
        ID_FILE_SAVE_AS => {
            save_document(state, true);
        }
        ID_FILE_PRINT => print_document(state),
        ID_FILE_EXIT => unsafe {
            SendMessageW(state.hwnd.get(), WM_CLOSE, 0, 0);
        },
        ID_EDIT_UNDO => unsafe {
            SendMessageW(state.editor.get(), WM_UNDO, 0, 0);
        },
        ID_EDIT_CUT => unsafe {
            SendMessageW(state.editor.get(), WM_CUT, 0, 0);
        },
        ID_EDIT_COPY => unsafe {
            SendMessageW(state.editor.get(), WM_COPY, 0, 0);
        },
        ID_EDIT_PASTE => unsafe {
            SendMessageW(state.editor.get(), WM_PASTE, 0, 0);
        },
        ID_EDIT_DELETE => unsafe {
            SendMessageW(state.editor.get(), WM_CLEAR, 0, 0);
        },
        ID_EDIT_FIND => show_find_dialog(state, false),
        ID_EDIT_FIND_NEXT => find_next(state),
        ID_EDIT_FIND_PREVIOUS => find_previous(state),
        ID_EDIT_REPLACE => show_find_dialog(state, true),
        ID_EDIT_GOTO => go_to_line(state),
        ID_EDIT_SELECT_ALL => {
            unsafe { SendMessageW(state.editor.get(), EM_SETSEL, 0, -1) };
            update_status_position(state);
        }
        ID_EDIT_TIME_DATE => insert_time_date(state),
        ID_FORMAT_WRAP => toggle_word_wrap(state),
        ID_FORMAT_FONT => choose_font(state),
        ID_VIEW_ZOOM_IN => change_zoom(state, 1),
        ID_VIEW_ZOOM_OUT => change_zoom(state, -1),
        ID_VIEW_ZOOM_DEFAULT => change_zoom(
            state,
            DEFAULT_ZOOM_INDEX as i32 - state.zoom_index.get() as i32,
        ),
        ID_VIEW_STATUS => toggle_status(state),
        ID_HELP_ABOUT => dialogs::show_about(state.hwnd.get(), state.instance),
        _ => {}
    }
}

fn print_document(state: &AppState) {
    let hwnd = state.hwnd.get();
    let text = get_editor_text_utf16(state.editor.get());
    let font_choice = state.font_choice.get();
    let display_name = state.display_name();
    if let Err(message) = printing::print(hwnd, &text, font_choice, &display_name) {
        dialogs::show_error(Some(hwnd), &app_name(), &message);
    }
}

fn new_document(state: &AppState) {
    if !maybe_save(state) {
        return;
    }
    *state.path.borrow_mut() = None;
    state.format.set(TextFormat::default());
    set_editor_text(state, "");
    state.dirty.set(false);
    state.set_title();
}

fn open_document(state: &AppState) {
    if !maybe_save(state) {
        return;
    }
    let hwnd = state.hwnd.get();
    match dialogs::open_file(hwnd) {
        Ok(Some(path)) => open_path(state, path),
        Ok(None) => {}
        Err(message) => dialogs::show_error(Some(hwnd), &app_name(), &message),
    }
}

fn open_command_line_path(state: &AppState, path: PathBuf) {
    if path.is_file() {
        open_path(state, path);
    } else if !path.exists() {
        if dialogs::confirm_create(state.hwnd.get(), &path) {
            *state.path.borrow_mut() = Some(path);
            state.format.set(TextFormat::default());
            state.dirty.set(false);
            set_editor_text(state, "");
            state.set_title();
        }
    } else {
        dialogs::show_error_with_path(
            Some(state.hwnd.get()),
            &app_name(),
            IDS_COMMAND_LINE_NOT_FILE,
            &path,
        );
    }
}

fn open_path(state: &AppState, path: PathBuf) {
    match file::load(&path) {
        Ok(loaded) => {
            let mut text = loaded.text;
            let appended_log_entry = is_log_document(&text)
                && current_time_date()
                    .is_some_and(|timestamp| append_log_entry(&mut text, &timestamp));
            set_editor_text(state, &text);
            *state.path.borrow_mut() = Some(path);
            state.format.set(loaded.format);
            state.dirty.set(appended_log_entry);
            if appended_log_entry {
                let end = text.encode_utf16().count();
                unsafe {
                    let editor = state.editor.get();
                    SendMessageW(editor, EM_SETMODIFY, 1, 0);
                    SendMessageW(editor, EM_SETSEL, end, end as isize);
                    SendMessageW(editor, EM_SCROLLCARET, 0, 0);
                }
            }
            state.set_title();
            update_status(state);
        }
        Err(error) => dialogs::show_error(
            Some(state.hwnd.get()),
            &app_name(),
            &localized_error(IDS_OPEN_FILE_FAILED, error),
        ),
    }
}

fn save_document(state: &AppState, force_dialog: bool) -> bool {
    let current_path = state.path.borrow().clone();
    let hwnd = state.hwnd.get();
    let path = if force_dialog {
        match dialogs::save_file(hwnd, current_path.as_deref()) {
            Ok(Some(path)) => path,
            Ok(None) => return false,
            Err(message) => {
                dialogs::show_error(Some(hwnd), &app_name(), &message);
                return false;
            }
        }
    } else if let Some(path) = current_path {
        path
    } else {
        match dialogs::save_file(hwnd, None) {
            Ok(Some(path)) => path,
            Ok(None) => return false,
            Err(message) => {
                dialogs::show_error(Some(hwnd), &app_name(), &message);
                return false;
            }
        }
    };
    let text = get_editor_text(state.editor.get());
    match file::save(&path, &text, state.format.get()) {
        Ok(()) => {
            *state.path.borrow_mut() = Some(path);
            state.dirty.set(false);
            state.set_title();
            true
        }
        Err(error) => {
            dialogs::show_error(
                Some(hwnd),
                &app_name(),
                &localized_error(IDS_SAVE_FILE_FAILED, error),
            );
            false
        }
    }
}

fn maybe_save(state: &AppState) -> bool {
    if !state.dirty.get() {
        return true;
    }
    let display_name = state.display_name();
    match dialogs::confirm_save(state.hwnd.get(), &display_name) {
        SaveDecision::Save => save_document(state, false),
        SaveDecision::Discard => true,
        SaveDecision::Cancel => false,
    }
}

fn set_editor_text(state: &AppState, text: &str) {
    let wide: Vec<u16> = text.encode_utf16().collect();
    set_editor_text_utf16(state, &wide);
}

fn set_editor_text_utf16(state: &AppState, text: &[u16]) {
    let mut terminated = Vec::with_capacity(text.len() + 1);
    terminated.extend_from_slice(text);
    terminated.push(0);
    let editor = state.editor.get();
    state.suppress_change.set(true);
    unsafe {
        SetWindowTextW(editor, terminated.as_ptr());
        SendMessageW(editor, EM_SETMODIFY, 0, 0);
        SendMessageW(editor, EM_SETSEL, 0, 0);
    }
    state.suppress_change.set(false);
    update_status(state);
}

fn get_editor_text(editor: HWND) -> String {
    String::from_utf16_lossy(&get_editor_text_utf16(editor))
}

fn get_editor_text_utf16(editor: HWND) -> Vec<u16> {
    let length = unsafe { GetWindowTextLengthW(editor) };
    if length <= 0 {
        return Vec::new();
    }
    let mut buffer = vec![0u16; length as usize + 1];
    let written = unsafe { GetWindowTextW(editor, buffer.as_mut_ptr(), buffer.len() as i32) };
    buffer.truncate(written.max(0) as usize);
    buffer
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

fn handle_drop(state: &AppState, drop: HDROP) {
    let length = unsafe { DragQueryFileW(drop, 0, null_mut(), 0) };
    if length > 0 {
        let mut buffer = vec![0u16; length as usize + 1];
        unsafe { DragQueryFileW(drop, 0, buffer.as_mut_ptr(), buffer.len() as u32) };
        let path = PathBuf::from(OsString::from_wide(&buffer[..length as usize]));
        if maybe_save(state) {
            open_path(state, path);
        }
    }
    unsafe { DragFinish(drop) };
}

fn toggle_word_wrap(state: &AppState) {
    let old_editor = state.editor.get();
    let text = get_editor_text_utf16(old_editor);
    let selected = selection(old_editor);
    let font = unsafe { SendMessageW(old_editor, WM_GETFONT, 0, 0) } as HFONT;
    state.word_wrap.set(!state.word_wrap.get());
    state.suppress_change.set(true);
    match create_editor(state) {
        Ok(new_editor) => {
            state.editor.set(new_editor);
            let mut terminated = text;
            terminated.push(0);
            unsafe {
                SendMessageW(new_editor, EM_SETLIMITTEXT, 0x7FFF_FFFE, 0);
                SendMessageW(new_editor, WM_SETFONT, font as usize, 1);
                SetWindowTextW(new_editor, terminated.as_ptr());
                SendMessageW(
                    new_editor,
                    EM_SETSEL,
                    selected.0 as usize,
                    selected.1 as isize,
                );
                SendMessageW(new_editor, EM_SETMODIFY, state.dirty.get() as usize, 0);
                DestroyWindow(old_editor);
                SetFocus(new_editor);
            }
        }
        Err(message) => {
            state.word_wrap.set(!state.word_wrap.get());
            dialogs::show_error(Some(state.hwnd.get()), &app_name(), &message);
        }
    }
    state.suppress_change.set(false);
    update_menu_state(state);
    layout_children(state);
    update_status(state);
}

fn toggle_status(state: &AppState) {
    if state.word_wrap.get() {
        return;
    }
    state.status_requested.set(!state.status_requested.get());
    update_menu_state(state);
    layout_children(state);
}

fn update_menu_state(state: &AppState) {
    unsafe {
        CheckMenuItem(
            state.menu,
            ID_FORMAT_WRAP as u32,
            MF_BYCOMMAND
                | if state.word_wrap.get() {
                    MF_CHECKED
                } else {
                    MF_UNCHECKED
                },
        );
        CheckMenuItem(
            state.menu,
            ID_VIEW_STATUS as u32,
            MF_BYCOMMAND
                | if state.status_requested.get() && !state.word_wrap.get() {
                    MF_CHECKED
                } else {
                    MF_UNCHECKED
                },
        );
        EnableMenuItem(
            state.menu,
            ID_VIEW_STATUS as u32,
            MF_BYCOMMAND
                | if state.word_wrap.get() {
                    MF_GRAYED
                } else {
                    MF_ENABLED
                },
        );
        EnableMenuItem(
            state.menu,
            ID_EDIT_GOTO as u32,
            MF_BYCOMMAND
                | if state.word_wrap.get() {
                    MF_GRAYED
                } else {
                    MF_ENABLED
                },
        );
        EnableMenuItem(
            state.menu,
            ID_VIEW_ZOOM_IN as u32,
            MF_BYCOMMAND
                | if state.zoom_index.get() + 1 == ZOOM_LEVELS.len() {
                    MF_GRAYED
                } else {
                    MF_ENABLED
                },
        );
        EnableMenuItem(
            state.menu,
            ID_VIEW_ZOOM_OUT as u32,
            MF_BYCOMMAND
                | if state.zoom_index.get() == 0 {
                    MF_GRAYED
                } else {
                    MF_ENABLED
                },
        );
        EnableMenuItem(
            state.menu,
            ID_VIEW_ZOOM_DEFAULT as u32,
            MF_BYCOMMAND
                | if state.zoom_index.get() == DEFAULT_ZOOM_INDEX {
                    MF_GRAYED
                } else {
                    MF_ENABLED
                },
        );
        DrawMenuBar(state.hwnd.get());
    }
}

fn update_status(state: &AppState) {
    let editor = state.editor.get();
    let status = state.status.get();
    if !state.status_is_visible() || editor.is_null() || status.is_null() {
        return;
    }
    let format = state.format.get();
    let zoom_text = localization::format(
        IDS_STATUS_ZOOM,
        &[FormatArg::Unsigned(
            zoom_percent(state.zoom_index.get()) as u64
        )],
    );
    let eol_text = localization::text(format.newline.status_resource_id());
    let encoding_text = localization::text(format.encoding.status_resource_id());

    let dpi = state.dpi.get();
    let zoom_width =
        measure_status_text_width(status, localization::without_trailing_nul(&zoom_text), dpi);
    let eol_width =
        measure_status_text_width(status, localization::without_trailing_nul(&eol_text), dpi);
    let enc_width = measure_status_text_width(
        status,
        localization::without_trailing_nul(&encoding_text),
        dpi,
    );

    let mut status_client: RECT = unsafe { zeroed() };
    unsafe { GetClientRect(status, &mut status_client) };
    let client_width = status_client.right - status_client.left;
    let parts = calculate_status_parts(client_width, &[zoom_width, eol_width, enc_width]);
    unsafe {
        SendMessageW(status, SB_SETPARTS, parts.len(), parts.as_ptr() as isize);
        SendMessageW(status, SB_SETTEXTW, 1, zoom_text.as_ptr() as isize);
        SendMessageW(status, SB_SETTEXTW, 2, eol_text.as_ptr() as isize);
        SendMessageW(status, SB_SETTEXTW, 3, encoding_text.as_ptr() as isize);
    }
    update_status_position(state);
}

fn update_status_position(state: &AppState) {
    let editor = state.editor.get();
    let status = state.status.get();
    if !state.status_is_visible() || editor.is_null() || status.is_null() {
        return;
    }
    let (caret, _) = selection(editor);
    let line = unsafe { SendMessageW(editor, EM_LINEFROMCHAR, caret as usize, 0) } as i32;
    let line_start = unsafe { SendMessageW(editor, EM_LINEINDEX, line as usize, 0) } as i32;
    let column = caret as i32 - line_start.max(0);
    let text = localization::format(
        IDS_STATUS_POSITION,
        &[
            FormatArg::Unsigned((line + 1) as u64),
            FormatArg::Unsigned((column + 1) as u64),
        ],
    );
    unsafe { SendMessageW(status, SB_SETTEXTW, 0, text.as_ptr() as isize) };
}

fn calculate_status_parts(client_width: i32, right_widths: &[i32]) -> Vec<i32> {
    if right_widths.is_empty() {
        return vec![-1];
    }
    let client_width = client_width.max(0);
    let mut parts = Vec::with_capacity(right_widths.len() + 1);
    let mut remaining_suffix: i32 = right_widths.iter().map(|&w| w.max(0)).sum();

    parts.push((client_width - remaining_suffix).max(0));
    for &width in &right_widths[..right_widths.len() - 1] {
        remaining_suffix = remaining_suffix.saturating_sub(width.max(0));
        parts.push((client_width - remaining_suffix).max(0));
    }
    parts.push(-1);
    parts
}

fn measure_status_text_width(status: HWND, text: &[u16], dpi: u32) -> i32 {
    let padding = ((i64::from(16) * i64::from(dpi.max(1)) + 48) / 96) as i32;
    if status.is_null() || text.is_empty() {
        return padding;
    }
    unsafe {
        let hdc = GetDC(status);
        if hdc.is_null() {
            return padding;
        }
        let font = SendMessageW(status, WM_GETFONT, 0, 0) as HFONT;
        let font = if font.is_null() {
            GetStockObject(DEFAULT_GUI_FONT) as HFONT
        } else {
            font
        };
        let old_font = SelectObject(hdc, font);
        let mut size: SIZE = zeroed();
        let success = GetTextExtentPoint32W(hdc, text.as_ptr(), text.len() as i32, &mut size);
        SelectObject(hdc, old_font);
        ReleaseDC(status, hdc);
        if success != 0 {
            size.cx.max(0) + padding
        } else {
            padding
        }
    }
}

fn choose_font(state: &AppState) {
    let hwnd = state.hwnd.get();
    let current = state.font_choice.get();
    let mut logical = current.logical;
    logical.lfHeight = rendered_font_height(current.point_size_tenths, state.dpi.get(), 100);
    let Some(point_size_tenths) = dialogs::choose_font(hwnd, &mut logical) else {
        return;
    };
    logical.lfHeight = 0;
    logical.lfWidth = 0;
    let choice = FontChoice {
        logical,
        point_size_tenths,
    };
    if !replace_editor_font_for(
        state,
        choice,
        state.dpi.get(),
        zoom_percent(state.zoom_index.get()),
    ) {
        let error = io::Error::last_os_error();
        dialogs::show_error(
            Some(hwnd),
            &app_name(),
            &dialogs::os_error(&localized_string(IDS_CREATE_FONT_FAILED), &error),
        );
        return;
    }
    state.font_choice.set(choice);
}

fn insert_time_date(state: &AppState) {
    if let Some(value) = current_time_date() {
        replace_selection(state.editor.get(), &value);
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

fn replace_selection_utf16(editor: HWND, text: &[u16]) {
    let mut terminated = Vec::with_capacity(text.len() + 1);
    terminated.extend_from_slice(text);
    terminated.push(0);
    unsafe { SendMessageW(editor, EM_REPLACESEL, 1, terminated.as_ptr() as isize) };
}

fn go_to_line(state: &AppState) {
    if state.word_wrap.get() {
        return;
    }
    let editor = state.editor.get();
    let hwnd = state.hwnd.get();
    let (caret, _) = selection(editor);
    let current = unsafe { SendMessageW(editor, EM_LINEFROMCHAR, caret as usize, 0) } as u32 + 1;
    if let Some(line) = dialogs::go_to_line(hwnd, state.instance, current) {
        let editor = state.editor.get();
        let line_count = unsafe { SendMessageW(editor, EM_GETLINECOUNT, 0, 0) } as u32;
        if line == 0 || line > line_count {
            dialogs::show_error(
                Some(hwnd),
                &localized_string(IDS_GOTO_TITLE),
                &localized_error(IDS_GOTO_RANGE, line_count),
            );
            return;
        }
        let index = unsafe { SendMessageW(editor, EM_LINEINDEX, (line - 1) as usize, 0) };
        unsafe {
            SendMessageW(editor, EM_SETSEL, index as usize, index);
            SendMessageW(editor, EM_SCROLLCARET, 0, 0);
            SetFocus(editor);
        }
        update_status_position(state);
    }
}

fn native_dialog_buffer(buffer: &UnsafeCell<[u16; 256]>) -> Vec<u16> {
    // SAFETY: the modeless common dialog and all Rust access run on the same UI
    // thread. No Win32 call can reenter while this in-place snapshot is copied.
    let snapshot = unsafe { buffer.get().read() };
    nul_terminated_slice(&snapshot).to_vec()
}

fn show_find_dialog(state: &AppState, replace: bool) {
    let existing = state.find_dialog.get();
    if !existing.is_null() {
        unsafe { SetForegroundWindow(existing) };
        return;
    }
    let editor = state.editor.get();
    if native_dialog_buffer(&state.find_text).is_empty() {
        let (start, end) = selection(editor);
        if end > start && end - start < 256 {
            let text = get_editor_text_utf16(editor);
            if end as usize <= text.len() {
                let selected = &text[start as usize..end as usize];
                // SAFETY: no modeless dialog exists yet, and the `Rc<AppState>`
                // allocation keeps this `UnsafeCell` at a stable address.
                unsafe {
                    let destination = state.find_text.get().cast::<u16>();
                    destination.copy_from_nonoverlapping(selected.as_ptr(), selected.len());
                    destination.add(selected.len()).write(0);
                }
            }
        }
    }
    let mut data = Box::<FINDREPLACEW>::default();
    data.lStructSize = size_of::<FINDREPLACEW>() as u32;
    data.hwndOwner = state.hwnd.get();
    data.Flags = state.find_flags.get();
    // SAFETY: these pointers target the arrays in the stable `AppState`
    // allocation. The arrays are never replaced while the dialog is alive.
    data.lpstrFindWhat = state.find_text.get().cast::<u16>();
    data.wFindWhatLen = 256;
    data.lpstrReplaceWith = state.replace_text.get().cast::<u16>();
    data.wReplaceWithLen = 256;
    let pointer = {
        let mut slot = state.find_data.borrow_mut();
        *slot = Some(data);
        slot.as_mut().unwrap().as_mut() as *mut FINDREPLACEW
    };
    // `pointer` remains stable in its `Box`; the RefCell borrow ended before
    // entering the modeless common-dialog API.
    let dialog = unsafe {
        if replace {
            ReplaceTextW(pointer)
        } else {
            FindTextW(pointer)
        }
    };
    state.find_dialog.set(dialog);
    if dialog.is_null() {
        state.find_data.borrow_mut().take();
        dialogs::show_error(
            Some(state.hwnd.get()),
            &app_name(),
            &localized_string(IDS_FIND_DIALOG_FAILED),
        );
    }
}

fn handle_find_message(state: &AppState, data: *const FINDREPLACEW) {
    if data.is_null() {
        return;
    }
    let flags = unsafe { (*data).Flags };
    if flags & FR_DIALOGTERM != 0 {
        state.find_dialog.set(null_mut());
        state.find_data.borrow_mut().take();
    } else {
        state.find_flags.set(flags & FIND_OPTION_FLAGS);
        if flags & FR_FINDNEXT != 0 {
            find_next_with_flags(state, flags);
        } else if flags & FR_REPLACE != 0 {
            replace_one(state, flags);
        } else if flags & FR_REPLACEALL != 0 {
            replace_all(state, flags);
        }
    }
}

fn find_next(state: &AppState) {
    state.find_flags.set(state.find_flags.get() | FR_DOWN);
    if native_dialog_buffer(&state.find_text).is_empty() {
        show_find_dialog(state, false);
        return;
    }
    let flags = state.find_flags.get() | FR_DOWN;
    find_next_with_flags(state, flags);
}

fn find_previous(state: &AppState) {
    state.find_flags.set(state.find_flags.get() & !FR_DOWN);
    if native_dialog_buffer(&state.find_text).is_empty() {
        show_find_dialog(state, false);
        return;
    }
    let flags = state.find_flags.get() & !FR_DOWN;
    find_next_with_flags(state, flags);
}

fn find_next_with_flags(state: &AppState, flags: u32) -> bool {
    let needle = native_dialog_buffer(&state.find_text);
    if needle.is_empty() {
        return false;
    }
    let editor = state.editor.get();
    let haystack = get_editor_text_utf16(editor);
    let (start, end) = selection(editor);
    let found = if flags & FR_DOWN != 0 {
        find_utf16(&haystack, &needle, end as usize, true, flags)
    } else {
        find_utf16(&haystack, &needle, start as usize, false, flags)
    };
    if let Some(index) = found {
        unsafe {
            SendMessageW(editor, EM_SETSEL, index, (index + needle.len()) as isize);
            SendMessageW(editor, EM_SCROLLCARET, 0, 0);
            SetFocus(editor);
        }
        update_status_position(state);
        true
    } else {
        let message = localization::format(IDS_FIND_NOT_FOUND, &[FormatArg::Wide(&needle)]);
        let title = dialogs::to_wide(&app_name());
        unsafe {
            MessageBoxW(
                state.hwnd.get(),
                message.as_ptr(),
                title.as_ptr(),
                MB_OK | MB_ICONINFORMATION,
            )
        };
        false
    }
}

fn replace_one(state: &AppState, flags: u32) {
    let needle = native_dialog_buffer(&state.find_text);
    let replacement = native_dialog_buffer(&state.replace_text);
    let editor = state.editor.get();
    let (start, end) = selection(editor);
    let text = get_editor_text_utf16(editor);
    if selection_matches(&text, start as usize, end as usize, &needle, flags) {
        replace_selection_utf16(editor, &replacement);
    }
    find_next_with_flags(state, flags);
}

fn selection_matches(text: &[u16], start: usize, end: usize, needle: &[u16], flags: u32) -> bool {
    end > start
        && end <= text.len()
        && slices_equal(&text[start..end], needle, flags)
        && (flags & FR_WHOLEWORD == 0 || whole_word_at(text, start, needle.len()))
}

fn replace_all(state: &AppState, flags: u32) {
    let needle = native_dialog_buffer(&state.find_text);
    let replacement = native_dialog_buffer(&state.replace_text);
    if needle.is_empty() {
        return;
    }
    let editor = state.editor.get();
    let input = get_editor_text_utf16(editor);
    let (output, count) = replace_all_utf16(&input, &needle, &replacement, flags);
    if count > 0 {
        set_editor_text_utf16(state, &output);
        state.dirty.set(true);
        unsafe { SendMessageW(editor, EM_SETMODIFY, 1, 0) };
        state.set_title();
    }
    let message = localization::format(IDS_REPLACE_COUNT, &[FormatArg::Unsigned(count as u64)]);
    let title = dialogs::to_wide(&app_name());
    unsafe {
        MessageBoxW(
            state.hwnd.get(),
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
    !scalar_before(text, start).is_some_and(is_word_scalar)
        && !scalar_after(text, start + length).is_some_and(is_word_scalar)
}

fn scalar_before(text: &[u16], boundary: usize) -> Option<char> {
    let last = *text.get(boundary.checked_sub(1)?)?;
    if is_low_surrogate(last) {
        let high = *text.get(boundary.checked_sub(2)?)?;
        decode_surrogate_pair(high, last)
    } else if is_high_surrogate(last) {
        None
    } else {
        char::from_u32(last as u32)
    }
}

fn scalar_after(text: &[u16], boundary: usize) -> Option<char> {
    let first = *text.get(boundary)?;
    if is_high_surrogate(first) {
        let low = *text.get(boundary + 1)?;
        decode_surrogate_pair(first, low)
    } else if is_low_surrogate(first) {
        None
    } else {
        char::from_u32(first as u32)
    }
}

const fn is_high_surrogate(unit: u16) -> bool {
    unit >= 0xD800 && unit <= 0xDBFF
}

const fn is_low_surrogate(unit: u16) -> bool {
    unit >= 0xDC00 && unit <= 0xDFFF
}

fn decode_surrogate_pair(high: u16, low: u16) -> Option<char> {
    if !is_high_surrogate(high) || !is_low_surrogate(low) {
        return None;
    }
    let scalar = 0x1_0000 + (((high as u32 - 0xD800) << 10) | (low as u32 - 0xDC00));
    char::from_u32(scalar)
}

fn is_word_scalar(ch: char) -> bool {
    ch.is_alphanumeric() || ch == '_'
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

    #[test]
    fn whole_word_boundaries_decode_supplementary_alphanumeric_scalars() {
        let before: Vec<u16> = "𐐀cat".encode_utf16().collect();
        let after: Vec<u16> = "cat𐐀".encode_utf16().collect();
        let separated: Vec<u16> = "𐐀 cat".encode_utf16().collect();

        assert!(!whole_word_at(&before, 2, 3));
        assert!(!whole_word_at(&after, 0, 3));
        assert!(whole_word_at(&separated, 3, 3));

        let needle: Vec<u16> = "cat".encode_utf16().collect();
        assert_eq!(
            find_utf16(&separated, &needle, 0, true, FR_DOWN | FR_WHOLEWORD,),
            Some(3)
        );
    }

    #[test]
    fn isolated_surrogates_are_non_word_boundaries() {
        let before = [0xD800, b'c' as u16, b'a' as u16, b't' as u16];
        let after = [b'c' as u16, b'a' as u16, b't' as u16, 0xDC00];

        assert!(whole_word_at(&before, 1, 3));
        assert!(whole_word_at(&after, 0, 3));
        assert_eq!(scalar_before(&before, 1), None);
        assert_eq!(scalar_after(&after, 3), None);
    }

    #[test]
    fn default_zoom_is_100_percent() {
        assert_eq!(zoom_percent(DEFAULT_ZOOM_INDEX), 100);
    }

    #[test]
    fn zoom_steps_through_the_fixed_scale() {
        assert_eq!(zoom_percent(stepped_zoom_index(DEFAULT_ZOOM_INDEX, 1)), 110);
        assert_eq!(zoom_percent(stepped_zoom_index(DEFAULT_ZOOM_INDEX, -1)), 90);
        assert_eq!(zoom_percent(stepped_zoom_index(DEFAULT_ZOOM_INDEX, 3)), 130);
        assert_eq!(zoom_percent(stepped_zoom_index(DEFAULT_ZOOM_INDEX, -3)), 70);
    }

    #[test]
    fn zoom_clamps_at_both_ends() {
        assert_eq!(stepped_zoom_index(0, -1), 0);
        assert_eq!(stepped_zoom_index(0, -99), 0);
        assert_eq!(
            stepped_zoom_index(ZOOM_LEVELS.len() - 1, 1),
            ZOOM_LEVELS.len() - 1
        );
        assert_eq!(
            stepped_zoom_index(ZOOM_LEVELS.len() - 1, 99),
            ZOOM_LEVELS.len() - 1
        );
    }

    #[test]
    fn wheel_delta_accumulates_small_positive_inputs() {
        let (steps, remainder) = accumulate_wheel_delta(0, 30);
        assert_eq!((steps, remainder), (0, 30));
        let (steps, remainder) = accumulate_wheel_delta(remainder, 30);
        assert_eq!((steps, remainder), (0, 60));
        assert_eq!(accumulate_wheel_delta(remainder, 60), (1, 0));
    }

    #[test]
    fn wheel_delta_accumulates_small_negative_inputs() {
        let (steps, remainder) = accumulate_wheel_delta(0, -40);
        assert_eq!((steps, remainder), (0, -40));
        let (steps, remainder) = accumulate_wheel_delta(remainder, -40);
        assert_eq!((steps, remainder), (0, -80));
        assert_eq!(accumulate_wheel_delta(remainder, -40), (-1, 0));
    }

    #[test]
    fn wheel_delta_handles_multiple_detents_and_direction_changes() {
        assert_eq!(accumulate_wheel_delta(0, 240), (2, 0));
        assert_eq!(accumulate_wheel_delta(100, -40), (0, 60));
        assert_eq!(accumulate_wheel_delta(60, -180), (-1, 0));
    }

    #[test]
    fn wheel_delta_keeps_only_a_bounded_remainder() {
        let (_, remainder) = accumulate_wheel_delta(i32::MAX, i32::MAX);
        assert!((-119..=119).contains(&remainder));
    }

    #[test]
    fn restore_default_zoom_returns_to_100_percent() {
        let current = stepped_zoom_index(DEFAULT_ZOOM_INDEX, 11);
        let restored = stepped_zoom_index(current, DEFAULT_ZOOM_INDEX as i32 - current as i32);
        assert_eq!(zoom_percent(restored), 100);
    }

    #[test]
    fn rendered_font_height_combines_logical_size_dpi_and_zoom() {
        assert_eq!(rendered_font_height(100, 96, 100), -13);
        assert_eq!(rendered_font_height(100, 96, 150), -20);
        assert_eq!(rendered_font_height(100, 192, 150), -40);
    }

    #[test]
    fn rendered_font_height_does_not_cumulate_scaling() {
        let at_150_percent = rendered_font_height(100, 96, 150);
        let after_dpi_change = rendered_font_height(100, 192, 150);
        let reset_at_original_dpi = rendered_font_height(100, 96, 100);

        assert_eq!(at_150_percent, -20);
        assert_eq!(after_dpi_change, -40);
        assert_eq!(reset_at_original_dpi, -13);
    }

    #[test]
    fn unusual_logical_font_sizes_are_scaled_safely() {
        assert_eq!(rendered_font_height(1, 96, 10), -1);
        assert_eq!(rendered_font_height(55, 144, 500), -55);
        assert_eq!(rendered_font_height(1234, 120, 250), -514);
    }

    #[test]
    fn calculate_status_parts_divides_normal_window_width() {
        let parts = calculate_status_parts(800, &[72, 120, 100]);
        assert_eq!(parts, vec![508, 580, 700, -1]);
    }

    #[test]
    fn calculate_status_parts_degrades_gracefully_in_narrow_windows() {
        let parts = calculate_status_parts(200, &[72, 120, 100]);
        assert_eq!(parts, vec![0, 0, 100, -1]);
    }

    #[test]
    fn calculate_status_parts_handles_zero_and_negative_client_widths() {
        let parts_zero = calculate_status_parts(0, &[72, 120, 100]);
        assert_eq!(parts_zero, vec![0, 0, 0, -1]);

        let parts_neg = calculate_status_parts(-50, &[72, 120, 100]);
        assert_eq!(parts_neg, vec![0, 0, 0, -1]);
    }

    #[test]
    fn calculate_status_parts_handles_empty_or_single_right_widths() {
        let parts_empty = calculate_status_parts(800, &[]);
        assert_eq!(parts_empty, vec![-1]);

        let parts_single = calculate_status_parts(800, &[72]);
        assert_eq!(parts_single, vec![728, -1]);
    }

    #[test]
    fn accelerators_table_creates_successfully_with_ctrl_p() {
        let table = create_accelerators().expect("accelerator table must create");
        assert_ne!(unsafe { DestroyAcceleratorTable(table) }, 0);
    }
}
