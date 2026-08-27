use std::mem::{size_of, zeroed};
use std::ptr::{null, null_mut};

use windows_sys::Win32::Foundation::{ERROR_SUCCESS, HWND, RECT};
use windows_sys::Win32::System::Registry::{
    HKEY, HKEY_CURRENT_USER, KEY_QUERY_VALUE, KEY_SET_VALUE, REG_DWORD, REG_OPTION_NON_VOLATILE,
    RegCloseKey, RegCreateKeyExW, RegOpenKeyExW, RegQueryValueExW, RegSetValueExW,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    GetWindowPlacement, SW_MAXIMIZE, SW_MINIMIZE, SW_SHOWMAXIMIZED, SW_SHOWMINIMIZED,
    SW_SHOWNORMAL, SetWindowPlacement, WINDOWPLACEMENT, WPF_RESTORETOMAXIMIZED,
};

pub const REGISTRY_SUBKEY: &str = "Software\\DeekFit\\Notepad Classic";
pub const VALUE_LEFT: &str = "WindowLeft";
pub const VALUE_TOP: &str = "WindowTop";
pub const VALUE_RIGHT: &str = "WindowRight";
pub const VALUE_BOTTOM: &str = "WindowBottom";
pub const VALUE_MAXIMIZED: &str = "WindowMaximized";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SavedPlacement {
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
    pub is_maximized: bool,
}

pub const fn coord_to_u32(coord: i32) -> u32 {
    coord as u32
}

pub const fn u32_to_coord(raw: u32) -> i32 {
    raw as i32
}

pub const fn is_valid_placement(
    left: i32,
    top: i32,
    right: i32,
    bottom: i32,
    maximized_raw: u32,
) -> bool {
    right > left && bottom > top && (maximized_raw == 0 || maximized_raw == 1)
}

pub fn validate_and_construct(
    left: i32,
    top: i32,
    right: i32,
    bottom: i32,
    maximized_raw: u32,
) -> Option<SavedPlacement> {
    if is_valid_placement(left, top, right, bottom, maximized_raw) {
        Some(SavedPlacement {
            left,
            top,
            right,
            bottom,
            is_maximized: maximized_raw == 1,
        })
    } else {
        None
    }
}

pub const fn normalize_show_state(show_cmd: u32, flags: u32) -> bool {
    if show_cmd == SW_SHOWMAXIMIZED as u32 || show_cmd == SW_MAXIMIZE as u32 {
        true
    } else if show_cmd == SW_SHOWMINIMIZED as u32 || show_cmd == SW_MINIMIZE as u32 {
        (flags & WPF_RESTORETOMAXIMIZED) != 0
    } else {
        false
    }
}

pub fn to_window_placement(saved: &SavedPlacement) -> WINDOWPLACEMENT {
    let mut wp: WINDOWPLACEMENT = unsafe { zeroed() };
    wp.length = size_of::<WINDOWPLACEMENT>() as u32;
    wp.flags = 0;
    wp.showCmd = if saved.is_maximized {
        SW_SHOWMAXIMIZED
    } else {
        SW_SHOWNORMAL
    } as u32;
    wp.rcNormalPosition = RECT {
        left: saved.left,
        top: saved.top,
        right: saved.right,
        bottom: saved.bottom,
    };
    wp
}

struct RegKey(HKEY);

impl Drop for RegKey {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe { RegCloseKey(self.0) };
        }
    }
}

unsafe fn read_dword(key: HKEY, value_name: &[u16]) -> Option<u32> {
    let mut data: u32 = 0;
    let mut data_type: u32 = 0;
    let mut data_len: u32 = size_of::<u32>() as u32;
    let status = unsafe {
        RegQueryValueExW(
            key,
            value_name.as_ptr(),
            null_mut(),
            &mut data_type,
            &mut data as *mut u32 as *mut u8,
            &mut data_len,
        )
    };
    if status == ERROR_SUCCESS && data_type == REG_DWORD && data_len == size_of::<u32>() as u32 {
        Some(data)
    } else {
        None
    }
}

unsafe fn write_dword(key: HKEY, value_name: &[u16], value: u32) -> bool {
    let status = unsafe {
        RegSetValueExW(
            key,
            value_name.as_ptr(),
            0,
            REG_DWORD,
            &value as *const u32 as *const u8,
            size_of::<u32>() as u32,
        )
    };
    status == ERROR_SUCCESS
}

pub fn load_window_placement() -> Option<SavedPlacement> {
    load_window_placement_from_key(HKEY_CURRENT_USER, REGISTRY_SUBKEY)
}

pub fn load_window_placement_from_key(root: HKEY, subkey: &str) -> Option<SavedPlacement> {
    let wide_subkey = crate::dialogs::to_wide(subkey);
    let mut hkey: HKEY = null_mut();
    let status =
        unsafe { RegOpenKeyExW(root, wide_subkey.as_ptr(), 0, KEY_QUERY_VALUE, &mut hkey) };
    if status != ERROR_SUCCESS || hkey.is_null() {
        return None;
    }
    let key = RegKey(hkey);

    let left_name = crate::dialogs::to_wide(VALUE_LEFT);
    let top_name = crate::dialogs::to_wide(VALUE_TOP);
    let right_name = crate::dialogs::to_wide(VALUE_RIGHT);
    let bottom_name = crate::dialogs::to_wide(VALUE_BOTTOM);
    let max_name = crate::dialogs::to_wide(VALUE_MAXIMIZED);

    let raw_left = unsafe { read_dword(key.0, &left_name)? };
    let raw_top = unsafe { read_dword(key.0, &top_name)? };
    let raw_right = unsafe { read_dword(key.0, &right_name)? };
    let raw_bottom = unsafe { read_dword(key.0, &bottom_name)? };
    let raw_max = unsafe { read_dword(key.0, &max_name)? };

    validate_and_construct(
        u32_to_coord(raw_left),
        u32_to_coord(raw_top),
        u32_to_coord(raw_right),
        u32_to_coord(raw_bottom),
        raw_max,
    )
}

pub fn save_window_placement(hwnd: HWND) {
    let _ = save_window_placement_to_key(hwnd, HKEY_CURRENT_USER, REGISTRY_SUBKEY);
}

pub fn save_window_placement_to_key(hwnd: HWND, root: HKEY, subkey: &str) -> bool {
    let mut wp: WINDOWPLACEMENT = unsafe { zeroed() };
    wp.length = size_of::<WINDOWPLACEMENT>() as u32;
    if unsafe { GetWindowPlacement(hwnd, &mut wp) } == 0 {
        return false;
    }

    let is_maximized = normalize_show_state(wp.showCmd, wp.flags);
    let rect = wp.rcNormalPosition;
    let max_val = if is_maximized { 1 } else { 0 };
    if !is_valid_placement(rect.left, rect.top, rect.right, rect.bottom, max_val) {
        return false;
    }

    let wide_subkey = crate::dialogs::to_wide(subkey);
    let mut hkey: HKEY = null_mut();
    let status = unsafe {
        RegCreateKeyExW(
            root,
            wide_subkey.as_ptr(),
            0,
            null_mut(),
            REG_OPTION_NON_VOLATILE,
            KEY_SET_VALUE,
            null(),
            &mut hkey,
            null_mut(),
        )
    };
    if status != ERROR_SUCCESS || hkey.is_null() {
        return false;
    }
    let key = RegKey(hkey);

    let left_name = crate::dialogs::to_wide(VALUE_LEFT);
    let top_name = crate::dialogs::to_wide(VALUE_TOP);
    let right_name = crate::dialogs::to_wide(VALUE_RIGHT);
    let bottom_name = crate::dialogs::to_wide(VALUE_BOTTOM);
    let max_name = crate::dialogs::to_wide(VALUE_MAXIMIZED);

    unsafe {
        let ok1 = write_dword(key.0, &left_name, coord_to_u32(rect.left));
        let ok2 = write_dword(key.0, &top_name, coord_to_u32(rect.top));
        let ok3 = write_dword(key.0, &right_name, coord_to_u32(rect.right));
        let ok4 = write_dword(key.0, &bottom_name, coord_to_u32(rect.bottom));
        let ok5 = write_dword(key.0, &max_name, max_val);
        ok1 && ok2 && ok3 && ok4 && ok5
    }
}

pub unsafe fn apply_window_placement(hwnd: HWND, saved: &SavedPlacement) -> bool {
    let wp = to_window_placement(saved);
    unsafe { SetWindowPlacement(hwnd, &wp) != 0 }
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows_sys::Win32::System::Registry::RegDeleteKeyW;
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        SW_NORMAL, SW_RESTORE, SW_SHOW, SW_SHOWMINIMIZED, SW_SHOWNORMAL,
    };

    #[test]
    fn coord_roundtrip_preserves_signed_values() {
        for coord in [i32::MIN, -1920, -1080, -1, 0, 1, 800, 1920, 3840, i32::MAX] {
            let encoded = coord_to_u32(coord);
            let decoded = u32_to_coord(encoded);
            assert_eq!(decoded, coord);
        }
    }

    #[test]
    fn negative_monitor_coordinates_are_valid() {
        let placement = validate_and_construct(-1920, -200, -100, 700, 0);
        assert_eq!(
            placement,
            Some(SavedPlacement {
                left: -1920,
                top: -200,
                right: -100,
                bottom: 700,
                is_maximized: false,
            })
        );
    }

    #[test]
    fn valid_rectangle_detection() {
        assert!(is_valid_placement(100, 100, 900, 700, 0));
        assert!(is_valid_placement(100, 100, 900, 700, 1));
        assert!(is_valid_placement(-500, 50, 100, 600, 0));
    }

    #[test]
    fn zero_and_inverted_rectangles_rejected() {
        // Zero width
        assert!(!is_valid_placement(100, 100, 100, 700, 0));
        // Zero height
        assert!(!is_valid_placement(100, 100, 900, 100, 0));
        // Inverted width
        assert!(!is_valid_placement(900, 100, 100, 700, 0));
        // Inverted height
        assert!(!is_valid_placement(100, 700, 900, 100, 0));
        // Invalid maximized flag
        assert!(!is_valid_placement(100, 100, 900, 700, 2));
        assert!(!is_valid_placement(100, 100, 900, 700, u32::MAX));
    }

    #[test]
    fn normal_state_normalization() {
        assert!(!normalize_show_state(SW_SHOWNORMAL as u32, 0));
        assert!(!normalize_show_state(SW_NORMAL as u32, 0));
        assert!(!normalize_show_state(SW_SHOW as u32, 0));
        assert!(!normalize_show_state(SW_RESTORE as u32, 0));
    }

    #[test]
    fn maximized_state_normalization() {
        assert!(normalize_show_state(SW_SHOWMAXIMIZED as u32, 0));
        assert!(normalize_show_state(SW_MAXIMIZE as u32, 0));
        assert!(normalize_show_state(
            SW_SHOWMAXIMIZED as u32,
            WPF_RESTORETOMAXIMIZED
        ));
    }

    #[test]
    fn minimized_from_normal_normalizes_to_normal() {
        assert!(!normalize_show_state(SW_SHOWMINIMIZED as u32, 0));
        assert!(!normalize_show_state(SW_MINIMIZE as u32, 0));
    }

    #[test]
    fn minimized_from_maximized_normalizes_to_maximized() {
        assert!(normalize_show_state(
            SW_SHOWMINIMIZED as u32,
            WPF_RESTORETOMAXIMIZED
        ));
        assert!(normalize_show_state(
            SW_MINIMIZE as u32,
            WPF_RESTORETOMAXIMIZED
        ));
    }

    #[test]
    fn to_window_placement_conversion() {
        let saved_normal = SavedPlacement {
            left: 50,
            top: 60,
            right: 850,
            bottom: 660,
            is_maximized: false,
        };
        let wp = to_window_placement(&saved_normal);
        assert_eq!(wp.length, size_of::<WINDOWPLACEMENT>() as u32);
        assert_eq!(wp.flags, 0);
        assert_eq!(wp.showCmd, SW_SHOWNORMAL as u32);
        assert_eq!(wp.rcNormalPosition.left, 50);
        assert_eq!(wp.rcNormalPosition.top, 60);
        assert_eq!(wp.rcNormalPosition.right, 850);
        assert_eq!(wp.rcNormalPosition.bottom, 660);

        let saved_maximized = SavedPlacement {
            left: -100,
            top: -200,
            right: 900,
            bottom: 800,
            is_maximized: true,
        };
        let wp_max = to_window_placement(&saved_maximized);
        assert_eq!(wp_max.flags, 0);
        assert_eq!(wp_max.showCmd, SW_SHOWMAXIMIZED as u32);
        assert_eq!(wp_max.rcNormalPosition.left, -100);
        assert_eq!(wp_max.rcNormalPosition.top, -200);
        assert_eq!(wp_max.rcNormalPosition.right, 900);
        assert_eq!(wp_max.rcNormalPosition.bottom, 800);
    }

    #[test]
    fn registry_load_missing_key_returns_none() {
        let missing = load_window_placement_from_key(
            HKEY_CURRENT_USER,
            "Software\\DeekFit\\Notepad Classic\\NonExistentTestKey_12345",
        );
        assert_eq!(missing, None);
    }

    #[test]
    fn registry_roundtrip_temporary_key() {
        let test_subkey = format!(
            "Software\\DeekFit\\Notepad Classic\\TestPlacement_{}",
            std::process::id()
        );
        let wide_test_subkey = crate::dialogs::to_wide(&test_subkey);

        // Ensure clean initial state
        let _ = unsafe { RegDeleteKeyW(HKEY_CURRENT_USER, wide_test_subkey.as_ptr()) };

        // Create and write test values manually
        let mut hkey: HKEY = null_mut();
        let status = unsafe {
            RegCreateKeyExW(
                HKEY_CURRENT_USER,
                wide_test_subkey.as_ptr(),
                0,
                null_mut(),
                REG_OPTION_NON_VOLATILE,
                KEY_SET_VALUE,
                null(),
                &mut hkey,
                null_mut(),
            )
        };
        assert_eq!(status, ERROR_SUCCESS);
        let key = RegKey(hkey);

        let left_name = crate::dialogs::to_wide(VALUE_LEFT);
        let top_name = crate::dialogs::to_wide(VALUE_TOP);
        let right_name = crate::dialogs::to_wide(VALUE_RIGHT);
        let bottom_name = crate::dialogs::to_wide(VALUE_BOTTOM);
        let max_name = crate::dialogs::to_wide(VALUE_MAXIMIZED);

        unsafe {
            assert!(write_dword(key.0, &left_name, coord_to_u32(-1500)));
            assert!(write_dword(key.0, &top_name, coord_to_u32(-100)));
            assert!(write_dword(key.0, &right_name, coord_to_u32(-300)));
            assert!(write_dword(key.0, &bottom_name, coord_to_u32(800)));
            assert!(write_dword(key.0, &max_name, 1));
        }

        // Read back
        let loaded = load_window_placement_from_key(HKEY_CURRENT_USER, &test_subkey);
        assert_eq!(
            loaded,
            Some(SavedPlacement {
                left: -1500,
                top: -100,
                right: -300,
                bottom: 800,
                is_maximized: true,
            })
        );

        // Clean up test key
        unsafe {
            let delete_status = RegDeleteKeyW(HKEY_CURRENT_USER, wide_test_subkey.as_ptr());
            assert_eq!(delete_status, ERROR_SUCCESS);
        }
    }
}
