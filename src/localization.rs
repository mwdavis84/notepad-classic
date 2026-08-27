//! The small runtime half of the embedded Win32 localization design.
//!
//! Windows performs the normal resource-language selection.  Only a failed
//! lookup is retried against the English (United States) resources compiled into this
//! executable, which keeps the portable EXE and MSIX builds identical.

use std::ffi::OsStr;
use std::mem::size_of;
use std::os::windows::ffi::OsStrExt;
use std::ptr::null;

use windows_sys::Win32::Foundation::{HINSTANCE, HRSRC};
use windows_sys::Win32::System::LibraryLoader::{
    FindResourceExW, GetModuleHandleW, LoadResource, LockResource, SizeofResource,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    HMENU, LoadMenuIndirectW, LoadMenuW, LoadStringW,
};

#[allow(dead_code)]
pub mod ids {
    include!(concat!(env!("OUT_DIR"), "/resource_ids.rs"));
}

const ENGLISH_US: u16 = 0x0409;
const RT_MENU: usize = 4;
const RT_STRING: usize = 6;

pub enum FormatArg<'a> {
    Wide(&'a [u16]),
    Os(&'a OsStr),
    Unsigned(u64),
}

pub fn string(id: usize) -> Option<Vec<u16>> {
    let instance = unsafe { GetModuleHandleW(null()) };
    if instance.is_null() {
        return None;
    }
    normal_string(instance, id).or_else(|| english_string(instance, id))
}

pub fn text(id: usize) -> Vec<u16> {
    string(id).unwrap_or_else(emergency_text)
}

pub fn without_trailing_nul(units: &[u16]) -> &[u16] {
    units.strip_suffix(&[0]).unwrap_or(units)
}

pub fn format(id: usize, args: &[FormatArg<'_>]) -> Vec<u16> {
    let template = text(id);
    format_units(without_trailing_nul(&template), args).unwrap_or_else(emergency_text)
}

pub fn menu(id: usize) -> Option<HMENU> {
    let instance = unsafe { GetModuleHandleW(null()) };
    if instance.is_null() {
        return None;
    }
    let menu = unsafe { LoadMenuW(instance, id as *const u16) };
    if !menu.is_null() {
        return Some(menu);
    }
    english_menu(instance, id)
}

fn english_menu(instance: HINSTANCE, id: usize) -> Option<HMENU> {
    let resource = unsafe {
        FindResourceExW(
            instance,
            RT_MENU as *const u16,
            id as *const u16,
            ENGLISH_US,
        )
    };
    if resource.is_null() {
        return None;
    }
    let loaded = unsafe { LoadResource(instance, resource) };
    if loaded.is_null() {
        return None;
    }
    let data = unsafe { LockResource(loaded) };
    if data.is_null() {
        return None;
    }
    // SAFETY: loaded resource memory remains valid for the lifetime of the
    // module. LoadMenuIndirectW consumes the template synchronously and does
    // not retain this pointer.
    let menu = unsafe { LoadMenuIndirectW(data) };
    (!menu.is_null()).then_some(menu)
}

fn normal_string(instance: HINSTANCE, id: usize) -> Option<Vec<u16>> {
    let mut capacity = 256usize;
    loop {
        let mut buffer = vec![0u16; capacity];
        let written =
            unsafe { LoadStringW(instance, id as u32, buffer.as_mut_ptr(), capacity as i32) };
        if written == 0 {
            return None;
        }
        if written as usize + 1 < capacity {
            buffer.truncate(written as usize);
            buffer.push(0);
            return Some(buffer);
        }
        capacity = capacity.checked_mul(2)?;
    }
}

fn english_string(instance: HINSTANCE, id: usize) -> Option<Vec<u16>> {
    let block = id.checked_div(16)?.checked_add(1)?;
    let resource = unsafe {
        FindResourceExW(
            instance,
            RT_STRING as *const u16,
            block as *const u16,
            ENGLISH_US,
        )
    };
    english_string_from_resource(instance, resource, id & 15)
}

fn english_string_from_resource(
    instance: HINSTANCE,
    resource: HRSRC,
    slot: usize,
) -> Option<Vec<u16>> {
    if resource.is_null() {
        return None;
    }
    let size = unsafe { SizeofResource(instance, resource) } as usize;
    if size < 2 || size % size_of::<u16>() != 0 {
        return None;
    }
    let loaded = unsafe { LoadResource(instance, resource) };
    if loaded.is_null() {
        return None;
    }
    let data = unsafe { LockResource(loaded) } as *const u16;
    if data.is_null() {
        return None;
    }
    // SAFETY: SizeofResource supplied the byte length, it was checked to be a
    // nonzero multiple of u16, and loaded resource memory remains module-owned.
    let units = unsafe { std::slice::from_raw_parts(data, size / 2) };
    let mut cursor = 0usize;
    let mut selected = None;
    for index in 0..16 {
        let length = *units.get(cursor)? as usize;
        cursor = cursor.checked_add(1)?;
        let end = cursor.checked_add(length)?;
        let value = units.get(cursor..end)?;
        if index == slot && !value.is_empty() {
            selected = Some(value.to_vec());
        }
        cursor = end;
    }
    let mut output = selected?;
    output.push(0);
    Some(output)
}

fn format_units(template: &[u16], args: &[FormatArg<'_>]) -> Option<Vec<u16>> {
    let mut output = Vec::with_capacity(template.len() + 1);
    let mut index = 0;
    while index < template.len() {
        if template[index] != b'%' as u16 {
            output.push(template[index]);
            index += 1;
            continue;
        }
        index += 1;
        if template.get(index) == Some(&(b'%' as u16)) {
            output.push(b'%' as u16);
            index += 1;
            continue;
        }
        let first = *template.get(index)?;
        if !(b'1' as u16..=b'9' as u16).contains(&first) {
            return None;
        }
        let mut number = 0usize;
        while let Some(unit) = template
            .get(index)
            .copied()
            .filter(|unit| (b'0' as u16..=b'9' as u16).contains(unit))
        {
            number = number
                .checked_mul(10)?
                .checked_add((unit - b'0' as u16) as usize)?;
            index += 1;
        }
        let argument = args.get(number.checked_sub(1)?)?;
        match argument {
            FormatArg::Wide(value) => output.extend_from_slice(value),
            FormatArg::Os(value) => output.extend(value.encode_wide()),
            FormatArg::Unsigned(value) => output.extend(value.to_string().encode_utf16()),
        }
    }
    output.push(0);
    Some(output)
}

fn emergency_text() -> Vec<u16> {
    "Notepad Classic could not load a required language resource."
        .encode_utf16()
        .chain(Some(0))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsString;
    use std::os::windows::ffi::{OsStrExt, OsStringExt};
    use windows_sys::Win32::UI::WindowsAndMessaging::DestroyMenu;

    #[test]
    fn formatting_supports_reordered_repeated_and_literal_percent_arguments() {
        let template: Vec<u16> = "%2 %1 %2 %%".encode_utf16().collect();
        let formatted = format_units(
            &template,
            &[FormatArg::Unsigned(7), FormatArg::Wide(&['é' as u16])],
        )
        .unwrap();
        assert_eq!(
            String::from_utf16_lossy(without_trailing_nul(&formatted)),
            "é 7 é %"
        );
    }

    #[test]
    fn embedded_english_catalog_contains_every_generated_string_and_menu() {
        for &id in ids::LOCALIZED_STRING_IDS {
            assert!(string(id).is_some(), "missing string resource {id}");
        }
        let menu = menu(ids::IDR_MAIN_MENU).expect("missing main menu resource");
        assert_ne!(unsafe { DestroyMenu(menu) }, 0);
    }

    #[test]
    fn explicit_english_string_fallback_matches_normal_loader_for_every_string() {
        let instance = unsafe { GetModuleHandleW(null()) };
        assert!(!instance.is_null());
        for &id in ids::LOCALIZED_STRING_IDS {
            let normal = normal_string(instance, id)
                .unwrap_or_else(|| panic!("normal loader could not load string {id}"));
            let fallback = english_string(instance, id)
                .unwrap_or_else(|| panic!("en-US fallback could not load string {id}"));
            assert_eq!(normal, fallback, "fallback mismatch for string {id}");
        }
    }

    #[test]
    fn explicit_english_menu_fallback_creates_the_main_menu() {
        let instance = unsafe { GetModuleHandleW(null()) };
        assert!(!instance.is_null());
        let menu = english_menu(instance, ids::IDR_MAIN_MENU)
            .expect("en-US fallback could not load the main menu");
        assert_ne!(unsafe { DestroyMenu(menu) }, 0);
    }

    #[test]
    fn formatting_preserves_unpaired_utf16_path_units() {
        let path = OsString::from_wide(&[b'C' as u16, b':' as u16, b'\\' as u16, 0xD800]);
        let template: Vec<u16> = "%1".encode_utf16().collect();
        let output = format_units(&template, &[FormatArg::Os(path.as_os_str())]).unwrap();
        assert_eq!(
            without_trailing_nul(&output),
            path.encode_wide().collect::<Vec<_>>()
        );
    }
}
