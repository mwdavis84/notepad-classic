use std::ptr::{null, null_mut};

use windows_sys::Win32::Foundation::ERROR_SUCCESS;
use windows_sys::Win32::Graphics::Gdi::LOGFONTW;
use windows_sys::Win32::System::Registry::{
    HKEY, HKEY_CURRENT_USER, KEY_QUERY_VALUE, KEY_SET_VALUE, REG_BINARY, REG_DWORD,
    REG_OPTION_NON_VOLATILE, RegCloseKey, RegCreateKeyExW, RegOpenKeyExW, RegQueryValueExW,
    RegSetValueExW,
};

use crate::app::FontChoice;
use crate::dialogs::to_wide;
use crate::window_placement::REGISTRY_SUBKEY;

const VALUE_NAME: &str = "EditorFont";
const DATA_SIZE: usize = 100;
// Application limit, shared with the font dialog to keep saved selections valid.
pub const MAX_FONT_POINTS: i32 = 1638;

struct RegKey(HKEY);

impl Drop for RegKey {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe { RegCloseKey(self.0) };
        }
    }
}

// A versioned, explicitly encoded record avoids persisting struct layout or
// DPI/zoom-dependent pixel dimensions. One registry value keeps the font and
// its point size together even if writing settings is interrupted.
fn encode(choice: FontChoice) -> [u8; DATA_SIZE] {
    let font = choice.logical;
    let mut data = [0; DATA_SIZE];
    let integers = [
        1,
        choice.point_size_tenths,
        0, // lfHeight is calculated from point size at the current DPI.
        0, // lfWidth lets the font mapper choose the natural width.
        font.lfEscapement,
        font.lfOrientation,
        font.lfWeight,
    ];
    for (chunk, value) in data[..28].chunks_exact_mut(4).zip(integers) {
        chunk.copy_from_slice(&value.to_le_bytes());
    }
    data[28..36].copy_from_slice(&[
        font.lfItalic,
        font.lfUnderline,
        font.lfStrikeOut,
        font.lfCharSet,
        font.lfOutPrecision,
        font.lfClipPrecision,
        font.lfQuality,
        font.lfPitchAndFamily,
    ]);
    for (chunk, unit) in data[36..].chunks_exact_mut(2).zip(font.lfFaceName) {
        chunk.copy_from_slice(&unit.to_le_bytes());
    }
    data
}

fn decode(data: &[u8]) -> Option<FontChoice> {
    if data.len() != DATA_SIZE {
        return None;
    }
    let integer = |offset| i32::from_le_bytes(data[offset..offset + 4].try_into().unwrap());
    let point_size_tenths = integer(4);
    if integer(0) != 1
        || !(1..=MAX_FONT_POINTS * 10).contains(&point_size_tenths)
        || !(0..=1000).contains(&integer(24))
    {
        return None;
    }
    let mut logical = LOGFONTW {
        lfEscapement: integer(16),
        lfOrientation: integer(20),
        lfWeight: integer(24),
        lfItalic: data[28],
        lfUnderline: data[29],
        lfStrikeOut: data[30],
        lfCharSet: data[31],
        lfOutPrecision: data[32],
        lfClipPrecision: data[33],
        lfQuality: data[34],
        lfPitchAndFamily: data[35],
        ..Default::default()
    };
    for (unit, chunk) in logical
        .lfFaceName
        .iter_mut()
        .zip(data[36..].chunks_exact(2))
    {
        *unit = u16::from_le_bytes(chunk.try_into().unwrap());
    }
    if logical.lfFaceName[0] == 0 || !logical.lfFaceName.contains(&0) {
        return None;
    }
    Some(FontChoice {
        logical,
        point_size_tenths,
    })
}

pub fn load_font() -> Option<FontChoice> {
    load_from_key(REGISTRY_SUBKEY)
}

fn load_from_key(subkey: &str) -> Option<FontChoice> {
    let mut data = [0; DATA_SIZE];
    if read_value(subkey, VALUE_NAME, &mut data)? != REG_BINARY {
        return None;
    }
    decode(&data)
}

fn read_value(subkey: &str, name: &str, data: &mut [u8]) -> Option<u32> {
    let subkey = to_wide(subkey);
    let mut key = null_mut();
    if unsafe {
        RegOpenKeyExW(
            HKEY_CURRENT_USER,
            subkey.as_ptr(),
            0,
            KEY_QUERY_VALUE,
            &mut key,
        )
    } != ERROR_SUCCESS
    {
        return None;
    }
    let key = RegKey(key);
    let name = to_wide(name);
    let mut data_type = 0;
    let mut size = data.len() as u32;
    let status = unsafe {
        RegQueryValueExW(
            key.0,
            name.as_ptr(),
            null_mut(),
            &mut data_type,
            data.as_mut_ptr(),
            &mut size,
        )
    };
    if status != ERROR_SUCCESS || size != data.len() as u32 {
        return None;
    }
    Some(data_type)
}

pub fn save_font(choice: FontChoice) {
    // Like window placement, unavailable settings storage must not stop editing.
    let _ = save_to_key(REGISTRY_SUBKEY, choice);
}

fn save_to_key(subkey: &str, choice: FontChoice) -> bool {
    write_value(subkey, VALUE_NAME, REG_BINARY, &encode(choice))
}

pub fn load_word_wrap() -> bool {
    read_bool(REGISTRY_SUBKEY, "WordWrap").unwrap_or(false)
}

pub fn load_status_bar() -> bool {
    read_bool(REGISTRY_SUBKEY, "StatusBar").unwrap_or(true)
}

fn read_bool(subkey: &str, name: &str) -> Option<bool> {
    let mut data = [0; 4];
    if read_value(subkey, name, &mut data)? != REG_DWORD {
        return None;
    }
    match u32::from_le_bytes(data) {
        0 => Some(false),
        1 => Some(true),
        _ => None,
    }
}

pub fn save_word_wrap(enabled: bool) {
    save_bool("WordWrap", enabled);
}

pub fn save_status_bar(enabled: bool) {
    save_bool("StatusBar", enabled);
}

fn save_bool(name: &str, enabled: bool) {
    let _ = write_value(
        REGISTRY_SUBKEY,
        name,
        REG_DWORD,
        &u32::from(enabled).to_le_bytes(),
    );
}

fn write_value(subkey: &str, name: &str, data_type: u32, data: &[u8]) -> bool {
    let subkey = to_wide(subkey);
    let mut key = null_mut();
    if unsafe {
        RegCreateKeyExW(
            HKEY_CURRENT_USER,
            subkey.as_ptr(),
            0,
            null_mut(),
            REG_OPTION_NON_VOLATILE,
            KEY_SET_VALUE,
            null(),
            &mut key,
            null_mut(),
        )
    } != ERROR_SUCCESS
    {
        return false;
    }
    let key = RegKey(key);
    let name = to_wide(name);
    unsafe {
        RegSetValueExW(
            key.0,
            name.as_ptr(),
            0,
            data_type,
            data.as_ptr(),
            data.len() as u32,
        ) == ERROR_SUCCESS
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::rendered_font_height;
    use windows_sys::Win32::System::Registry::RegDeleteKeyW;

    fn choice() -> FontChoice {
        let mut logical = LOGFONTW {
            lfHeight: -36,
            lfWeight: 700,
            lfItalic: 1,
            lfCharSet: 1,
            ..Default::default()
        };
        let face = to_wide("Consolas");
        logical.lfFaceName[..face.len()].copy_from_slice(&face);
        FontChoice {
            logical,
            point_size_tenths: 180,
        }
    }

    #[test]
    fn restored_font_keeps_size_and_style_across_dpi_and_zoom_changes() {
        let original = choice();
        let restored = decode(&encode(original)).unwrap();
        assert_eq!(restored.point_size_tenths, 180);
        assert_eq!(restored.logical.lfFaceName, original.logical.lfFaceName);
        assert_eq!(restored.logical.lfWeight, 700);
        assert_eq!(restored.logical.lfItalic, 1);
        assert_eq!(restored.logical.lfHeight, 0);
        assert_eq!(
            rendered_font_height(restored.point_size_tenths, 96, 100),
            -24
        );
        assert_eq!(
            rendered_font_height(restored.point_size_tenths, 144, 100),
            -36
        );
        assert_eq!(
            rendered_font_height(restored.point_size_tenths, 144, 150),
            -54
        );
    }

    #[test]
    fn malformed_settings_are_rejected() {
        let valid = encode(choice());
        assert!(decode(&valid[..99]).is_none());
        for (offset, value) in [
            (0, 2_i32),
            (4, 0),
            (4, -10),
            (4, MAX_FONT_POINTS * 10 + 1),
            (4, i32::MAX),
            (24, 1001),
        ] {
            let mut data = valid;
            data[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
            assert!(decode(&data).is_none());
        }
        let mut data = valid;
        data[36..].fill(1);
        assert!(decode(&data).is_none());
    }

    #[test]
    fn supported_point_size_boundaries_roundtrip() {
        for size in [1, 10, 125, MAX_FONT_POINTS * 10] {
            let mut font = choice();
            font.point_size_tenths = size;
            assert_eq!(decode(&encode(font)).unwrap().point_size_tenths, size);
        }
    }

    #[test]
    fn registry_roundtrip_and_missing_settings() {
        let subkey = format!("{REGISTRY_SUBKEY}\\TestFont_{}", std::process::id());
        struct TestKey(String);
        impl Drop for TestKey {
            fn drop(&mut self) {
                unsafe { RegDeleteKeyW(HKEY_CURRENT_USER, to_wide(&self.0).as_ptr()) };
            }
        }
        let cleanup = TestKey(subkey);
        assert!(load_from_key(&cleanup.0).is_none());
        assert_eq!(read_bool(&cleanup.0, "WordWrap"), None);
        assert_eq!(read_bool(&cleanup.0, "StatusBar"), None);
        assert!(save_to_key(&cleanup.0, choice()));
        let restored = load_from_key(&cleanup.0).unwrap();
        assert_eq!(encode(restored), encode(choice()));
        // A later confirmed selection replaces the previous preference.
        let mut updated = choice();
        updated.point_size_tenths = 125;
        assert!(save_to_key(&cleanup.0, updated));
        assert_eq!(load_from_key(&cleanup.0).unwrap().point_size_tenths, 125);

        // Each preference must survive independently, including when wrap is
        // enabled at exit.
        for wrap in [true, false] {
            assert!(write_value(
                &cleanup.0,
                "WordWrap",
                REG_DWORD,
                &u32::from(wrap).to_le_bytes()
            ));
            for status in [true, false] {
                assert!(write_value(
                    &cleanup.0,
                    "StatusBar",
                    REG_DWORD,
                    &u32::from(status).to_le_bytes()
                ));
                assert_eq!(read_bool(&cleanup.0, "WordWrap"), Some(wrap));
                assert_eq!(read_bool(&cleanup.0, "StatusBar"), Some(status));
                assert_eq!(load_from_key(&cleanup.0).unwrap().point_size_tenths, 125);
            }
        }
        // Bad types, lengths, and boolean values fall back to the default.
        for (data_type, data) in [
            (REG_BINARY, vec![1, 0, 0, 0]),
            (REG_DWORD, vec![1]),
            (REG_DWORD, vec![1, 0, 0, 0, 0]),
            (REG_DWORD, vec![2, 0, 0, 0]),
        ] {
            for name in ["WordWrap", "StatusBar"] {
                assert!(write_value(&cleanup.0, name, data_type, &data));
                assert_eq!(read_bool(&cleanup.0, name), None);
            }
        }
    }
}
