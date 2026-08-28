//! Native Windows printing implementation for Notepad Classic.
//!
//! The primary path uses `Windows.Graphics.Printing` so the system print UI can
//! request pagination, preview surfaces, and the final document. The proven
//! `PrintDlgW`/GDI implementation remains as a fallback when that API is not
//! available. Both paths use the logical unzoomed font choice.

mod modern;

use std::ffi::OsStr;
use std::io;
use std::mem::{size_of, zeroed};
use std::os::windows::ffi::OsStrExt;
use std::ptr::null;

use windows_sys::Win32::Foundation::{GlobalFree, HWND, SIZE};
use windows_sys::Win32::Graphics::Gdi::{
    CreateFontIndirectW, DeleteDC, DeleteObject, GetDeviceCaps, GetTextExtentPoint32W,
    GetTextMetricsW, HDC, HORZRES, LOGPIXELSX, LOGPIXELSY, SelectObject, TEXTMETRICW, TextOutW,
    VERTRES,
};
use windows_sys::Win32::UI::Controls::Dialogs::{
    CommDlgExtendedError, PD_HIDEPRINTTOFILE, PD_NOPAGENUMS, PD_NOSELECTION, PD_RETURNDC,
    PD_USEDEVMODECOPIESANDCOLLATE, PRINTDLGW, PrintDlgW,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{PostMessageW, WM_APP};

use crate::app::{FontChoice, rendered_font_height};
use crate::dialogs;
use crate::localization::ids::*;
use crate::localization::{self, FormatArg};

pub(crate) const WM_APP_PRINT_FAILURE: u32 = WM_APP + 2;

#[derive(Clone, Copy)]
pub(super) enum AsyncPrintFailure {
    Initialization = 1,
    Rendering = 2,
}

pub(super) fn post_async_failure(owner: HWND, failure: AsyncPrintFailure) {
    unsafe {
        PostMessageW(owner, WM_APP_PRINT_FAILURE, failure as usize, 0);
    }
}

pub(crate) fn show_async_failure(owner: HWND, failure: usize) {
    let message = if failure == AsyncPrintFailure::Rendering as usize {
        localized_error(
            IDS_PRINT_JOB_FAILED,
            localized_string(IDS_PRINT_RENDER_FAILED),
        )
    } else {
        localized_string(IDS_PRINT_INIT_FAILED)
    };
    dialogs::show_error(Some(owner), &localized_string(IDS_APP_NAME), &message);
}

#[repr(C)]
#[allow(non_snake_case, clippy::upper_case_acronyms)]
pub struct DOCINFOW {
    pub cbSize: i32,
    pub lpszDocName: *const u16,
    pub lpszOutput: *const u16,
    pub lpszDatatype: *const u16,
    pub fwType: u32,
}

#[link(name = "gdi32")]
unsafe extern "system" {
    fn StartDocW(hdc: HDC, lpdi: *const DOCINFOW) -> i32;
    fn StartPage(hdc: HDC) -> i32;
    fn EndPage(hdc: HDC) -> i32;
    fn EndDoc(hdc: HDC) -> i32;
    fn AbortDoc(hdc: HDC) -> i32;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PageGeometry {
    pub page_width: i32,
    pub page_height: i32,
    pub margin_x: i32,
    pub margin_y: i32,
    pub content_width: i32,
    pub content_height: i32,
    pub dpi_x: u32,
    pub dpi_y: u32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PrintedPage {
    pub lines: Vec<Vec<u16>>,
}

pub fn query_page_geometry(hdc: HDC) -> PageGeometry {
    let dpi_x = unsafe { GetDeviceCaps(hdc, LOGPIXELSX as i32) as u32 }.max(1);
    let dpi_y = unsafe { GetDeviceCaps(hdc, LOGPIXELSY as i32) as u32 }.max(1);
    let page_width = unsafe { GetDeviceCaps(hdc, HORZRES as i32) };
    let page_height = unsafe { GetDeviceCaps(hdc, VERTRES as i32) };

    // Minimal physical margin: 0.25 in (1/4") converted via printer DPI
    let mut margin_x = ((i64::from(dpi_x) * 25 + 50) / 100) as i32;
    let mut margin_y = ((i64::from(dpi_y) * 25 + 50) / 100) as i32;

    let mut content_width = page_width - 2 * margin_x;
    let mut content_height = page_height - 2 * margin_y;

    if content_width <= 0 {
        margin_x = 0;
        content_width = page_width.max(1);
    }
    if content_height <= 0 {
        margin_y = 0;
        content_height = page_height.max(1);
    }

    PageGeometry {
        page_width,
        page_height,
        margin_x,
        margin_y,
        content_width,
        content_height,
        dpi_x,
        dpi_y,
    }
}

pub fn split_logical_lines(text: &[u16]) -> Vec<&[u16]> {
    let mut lines = Vec::new();
    let mut start = 0;
    let mut i = 0;
    while i < text.len() {
        if text[i] == b'\r' as u16 {
            lines.push(&text[start..i]);
            if i + 1 < text.len() && text[i + 1] == b'\n' as u16 {
                i += 2;
            } else {
                i += 1;
            }
            start = i;
        } else if text[i] == b'\n' as u16 {
            lines.push(&text[start..i]);
            i += 1;
            start = i;
        } else {
            i += 1;
        }
    }
    if start <= text.len() {
        lines.push(&text[start..]);
    }
    lines
}

pub fn expand_tabs(line: &[u16]) -> Vec<u16> {
    let mut result = Vec::with_capacity(line.len());
    let mut col = 0usize;
    for &unit in line {
        if unit == b'\t' as u16 {
            let spaces = 8 - (col % 8);
            result.resize(result.len() + spaces, b' ' as u16);
            col += spaces;
        } else {
            result.push(unit);
            col += 1;
        }
    }
    result
}

pub fn wrap_line<F>(line: &[u16], max_width: i32, measure_width: &F) -> Option<Vec<Vec<u16>>>
where
    F: Fn(&[u16]) -> Option<i32>,
{
    if line.is_empty() {
        return Some(vec![Vec::new()]);
    }

    let mut wrapped_lines = Vec::new();
    let mut remaining = line;

    while !remaining.is_empty() {
        if measure_width(remaining)? <= max_width {
            wrapped_lines.push(remaining.to_vec());
            break;
        }

        let fit_count = find_fitting_prefix_len(remaining, max_width, measure_width)?;

        if fit_count >= remaining.len() {
            wrapped_lines.push(remaining.to_vec());
            break;
        }

        let break_point = find_whitespace_break(remaining, fit_count);

        let (current_line, next_remaining) = match break_point {
            Some(space_idx) => {
                let mut line_end = space_idx;
                while line_end > 0 && remaining[line_end - 1] == b' ' as u16 {
                    line_end -= 1;
                }
                let line_content = &remaining[..line_end];
                let mut next_start = space_idx + 1;
                while next_start < remaining.len() && remaining[next_start] == b' ' as u16 {
                    next_start += 1;
                }
                (line_content.to_vec(), &remaining[next_start..])
            }
            None => {
                let mut count = fit_count.max(1);
                if count < remaining.len()
                    && is_high_surrogate(remaining[count - 1])
                    && is_low_surrogate(remaining[count])
                {
                    if count == 1 {
                        count = 2;
                    } else {
                        count -= 1;
                    }
                } else if count == 1
                    && is_high_surrogate(remaining[0])
                    && remaining.len() > 1
                    && is_low_surrogate(remaining[1])
                {
                    count = 2;
                }
                let count = count.min(remaining.len());
                (remaining[..count].to_vec(), &remaining[count..])
            }
        };

        wrapped_lines.push(current_line);
        remaining = next_remaining;
    }

    if wrapped_lines.is_empty() {
        wrapped_lines.push(Vec::new());
    }

    Some(wrapped_lines)
}

fn find_fitting_prefix_len<F>(text: &[u16], max_width: i32, measure_width: &F) -> Option<usize>
where
    F: Fn(&[u16]) -> Option<i32>,
{
    let mut low = 1;
    let mut high = text.len();
    let mut best = 0;

    while low <= high {
        let mid = low + (high - low) / 2;
        let measure_len = if mid < text.len()
            && is_high_surrogate(text[mid - 1])
            && is_low_surrogate(text[mid])
        {
            mid + 1
        } else {
            mid
        };

        let width = measure_width(&text[..measure_len])?;
        if width <= max_width {
            best = measure_len;
            low = measure_len + 1;
        } else {
            if mid <= 1 {
                break;
            }
            high = mid - 1;
        }
    }

    Some(best)
}

fn find_whitespace_break(text: &[u16], fit_count: usize) -> Option<usize> {
    if fit_count == 0 || fit_count > text.len() {
        return None;
    }
    if fit_count < text.len() && text[fit_count] == b' ' as u16 {
        return Some(fit_count);
    }
    (1..fit_count).rev().find(|&i| text[i] == b' ' as u16)
}

const fn is_high_surrogate(unit: u16) -> bool {
    unit >= 0xD800 && unit <= 0xDBFF
}

const fn is_low_surrogate(unit: u16) -> bool {
    unit >= 0xDC00 && unit <= 0xDFFF
}

pub fn paginate<F>(
    text: &[u16],
    max_width: i32,
    lines_per_page: usize,
    measure_width: F,
) -> Option<Vec<PrintedPage>>
where
    F: Fn(&[u16]) -> Option<i32>,
{
    let logical_lines = split_logical_lines(text);
    let mut visual_lines = Vec::new();

    for line in logical_lines {
        let expanded = expand_tabs(line);
        let wrapped = wrap_line(&expanded, max_width, &measure_width)?;
        visual_lines.extend(wrapped);
    }

    if visual_lines.is_empty() {
        return Some(vec![PrintedPage {
            lines: vec![Vec::new()],
        }]);
    }

    let lines_per_page = lines_per_page.max(1);
    let mut pages = Vec::new();

    for chunk in visual_lines.chunks(lines_per_page) {
        pages.push(PrintedPage {
            lines: chunk.to_vec(),
        });
    }

    if pages.is_empty() {
        pages.push(PrintedPage {
            lines: vec![Vec::new()],
        });
    }

    Some(pages)
}

pub fn print(
    owner: HWND,
    text: &[u16],
    font_choice: FontChoice,
    display_name: &OsStr,
) -> Result<(), String> {
    match modern::show_print_ui(owner, text, font_choice, display_name) {
        Ok(()) => return Ok(()),
        Err(modern::ModernPrintError::Unavailable) => {}
        Err(modern::ModernPrintError::Failed(_error)) => {
            return Err(localized_string(IDS_PRINT_INIT_FAILED));
        }
    }

    legacy_print(owner, text, font_choice, display_name)
}

/// Balances a deferred WinRT initialization on the application UI thread.
pub fn shutdown() {
    modern::shutdown();
}

fn legacy_print(
    owner: HWND,
    text: &[u16],
    font_choice: FontChoice,
    display_name: &OsStr,
) -> Result<(), String> {
    let mut pd: PRINTDLGW = unsafe { zeroed() };
    pd.lStructSize = size_of::<PRINTDLGW>() as u32;
    pd.hwndOwner = owner;
    pd.Flags = PD_RETURNDC
        | PD_NOPAGENUMS
        | PD_NOSELECTION
        | PD_HIDEPRINTTOFILE
        | PD_USEDEVMODECOPIESANDCOLLATE;
    pd.nFromPage = 1;
    pd.nToPage = 1;
    pd.nMinPage = 1;
    pd.nMaxPage = 1;
    pd.nCopies = 1;

    let dialog_result = unsafe { PrintDlgW(&mut pd) };

    let dev_mode = pd.hDevMode;
    let dev_names = pd.hDevNames;
    let hdc = pd.hDC;

    if !dev_mode.is_null() {
        unsafe { GlobalFree(dev_mode) };
    }
    if !dev_names.is_null() {
        unsafe { GlobalFree(dev_names) };
    }

    if dialog_result == 0 {
        let error = unsafe { CommDlgExtendedError() };
        if !hdc.is_null() {
            unsafe { DeleteDC(hdc) };
        }
        if error == 0 {
            // User cancelled; not an error
            return Ok(());
        }
        let detail = format!("0x{error:08X}").encode_utf16().collect::<Vec<_>>();
        return Err(localized_format(
            IDS_PRINT_DIALOG_FAILED,
            &[FormatArg::Wide(&detail)],
        ));
    }

    if hdc.is_null() {
        return Err(localized_string(IDS_PRINT_INIT_FAILED));
    }

    let result = render_print_job(hdc, text, font_choice, display_name);
    unsafe { DeleteDC(hdc) };
    result
}

fn render_print_job(
    hdc: HDC,
    text: &[u16],
    font_choice: FontChoice,
    display_name: &OsStr,
) -> Result<(), String> {
    let geometry = query_page_geometry(hdc);

    let mut logical = font_choice.logical;
    logical.lfHeight = rendered_font_height(font_choice.point_size_tenths, geometry.dpi_y, 100);
    logical.lfWidth = 0;

    let printer_font = unsafe { CreateFontIndirectW(&logical) };
    if printer_font.is_null() {
        let error = io::Error::last_os_error();
        return Err(localized_error(IDS_CREATE_FONT_FAILED, error));
    }

    let previous_font = unsafe { SelectObject(hdc, printer_font) };
    if previous_font.is_null() {
        unsafe { DeleteObject(printer_font) };
        return Err(localized_string(IDS_PRINT_INIT_FAILED));
    }

    let mut tm: TEXTMETRICW = unsafe { zeroed() };
    if unsafe { GetTextMetricsW(hdc, &mut tm) } == 0 {
        let error = io::Error::last_os_error();
        unsafe {
            SelectObject(hdc, previous_font);
            DeleteObject(printer_font);
        }
        return Err(localized_error(IDS_PRINT_INIT_FAILED, error));
    }

    let line_height = (tm.tmHeight + tm.tmExternalLeading).max(1);
    let lines_per_page = (geometry.content_height / line_height).max(1) as usize;

    let measure_width = |slice: &[u16]| -> Option<i32> {
        if slice.is_empty() {
            return Some(0);
        }
        let mut size: SIZE = unsafe { zeroed() };
        let ok =
            unsafe { GetTextExtentPoint32W(hdc, slice.as_ptr(), slice.len() as i32, &mut size) };
        if ok != 0 { Some(size.cx.max(0)) } else { None }
    };

    let Some(pages) = paginate(text, geometry.content_width, lines_per_page, measure_width) else {
        unsafe {
            SelectObject(hdc, previous_font);
            DeleteObject(printer_font);
        }
        let detail = localized_string(IDS_PRINT_MEASURE_FAILED);
        return Err(localized_error(IDS_PRINT_JOB_FAILED, detail));
    };

    let doc_name = to_wide_os(display_name);

    let doc_info = DOCINFOW {
        cbSize: size_of::<DOCINFOW>() as i32,
        lpszDocName: doc_name.as_ptr(),
        lpszOutput: null(),
        lpszDatatype: null(),
        fwType: 0,
    };

    let job_id = unsafe { StartDocW(hdc, &doc_info) };
    if job_id <= 0 {
        let error = io::Error::last_os_error();
        unsafe {
            SelectObject(hdc, previous_font);
            DeleteObject(printer_font);
        }
        return Err(localized_error(IDS_PRINT_JOB_FAILED, error));
    }

    enum PageFailure {
        StartPage(io::Error),
        RenderText,
        EndPage(io::Error),
    }

    let mut page_failure: Option<PageFailure> = None;

    for page in &pages {
        if unsafe { StartPage(hdc) } <= 0 {
            page_failure = Some(PageFailure::StartPage(io::Error::last_os_error()));
            break;
        }

        for (line_idx, line) in page.lines.iter().enumerate() {
            if line.is_empty() {
                continue;
            }
            let x = geometry.margin_x;
            let y = geometry.margin_y + (line_idx as i32) * line_height;
            let written = unsafe { TextOutW(hdc, x, y, line.as_ptr(), line.len() as i32) };
            if written == 0 {
                page_failure = Some(PageFailure::RenderText);
                break;
            }
        }

        if page_failure.is_some() {
            break;
        }

        if unsafe { EndPage(hdc) } <= 0 {
            page_failure = Some(PageFailure::EndPage(io::Error::last_os_error()));
            break;
        }
    }

    if let Some(failure) = page_failure {
        unsafe {
            AbortDoc(hdc);
            SelectObject(hdc, previous_font);
            DeleteObject(printer_font);
        }
        return match failure {
            PageFailure::StartPage(error) | PageFailure::EndPage(error) => {
                Err(localized_error(IDS_PRINT_JOB_FAILED, error))
            }
            PageFailure::RenderText => {
                let detail = localized_string(IDS_PRINT_RENDER_FAILED);
                Err(localized_error(IDS_PRINT_JOB_FAILED, detail))
            }
        };
    }

    if unsafe { EndDoc(hdc) } <= 0 {
        let error = io::Error::last_os_error();
        unsafe {
            SelectObject(hdc, previous_font);
            DeleteObject(printer_font);
        }
        return Err(localized_error(IDS_PRINT_JOB_FAILED, error));
    }

    unsafe {
        SelectObject(hdc, previous_font);
        DeleteObject(printer_font);
    }

    Ok(())
}

fn to_wide_os(text: &OsStr) -> Vec<u16> {
    text.encode_wide().chain(Some(0)).collect()
}

fn localized_string(id: usize) -> String {
    let text = localization::text(id);
    String::from_utf16_lossy(localization::without_trailing_nul(&text))
}

fn localized_format(id: usize, args: &[FormatArg<'_>]) -> String {
    let text = localization::format(id, args);
    String::from_utf16_lossy(localization::without_trailing_nul(&text))
}

fn localized_error(id: usize, detail: impl std::fmt::Display) -> String {
    let detail = detail.to_string().encode_utf16().collect::<Vec<_>>();
    let text = localization::format(id, &[FormatArg::Wide(&detail)]);
    String::from_utf16_lossy(localization::without_trailing_nul(&text))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_text_paginates_to_single_page() {
        let pages = paginate(&[], 100, 10, |_| Some(10)).unwrap();
        assert_eq!(pages.len(), 1);
        assert_eq!(pages[0].lines, vec![Vec::<u16>::new()]);
    }

    #[test]
    fn short_line_fits_on_one_page() {
        let text: Vec<u16> = "Hello".encode_utf16().collect();
        let pages = paginate(&text, 100, 10, |s| Some(s.len() as i32 * 10)).unwrap();
        assert_eq!(pages.len(), 1);
        assert_eq!(pages[0].lines.len(), 1);
        assert_eq!(pages[0].lines[0], text);
    }

    #[test]
    fn explicit_blank_lines_are_preserved() {
        let text: Vec<u16> = "Line 1\r\n\r\nLine 2".encode_utf16().collect();
        let pages = paginate(&text, 200, 10, |s| Some(s.len() as i32 * 10)).unwrap();
        assert_eq!(pages.len(), 1);
        assert_eq!(pages[0].lines.len(), 3);
        assert_eq!(String::from_utf16_lossy(&pages[0].lines[0]), "Line 1");
        assert_eq!(pages[0].lines[1], Vec::<u16>::new());
        assert_eq!(String::from_utf16_lossy(&pages[0].lines[2]), "Line 2");
    }

    #[test]
    fn multiple_logical_lines_are_split_with_crlf_lf_and_cr() {
        let text: Vec<u16> = "A\r\nB\nC\rD".encode_utf16().collect();
        let lines = split_logical_lines(&text);
        assert_eq!(lines.len(), 4);
        assert_eq!(lines[0], &['A' as u16]);
        assert_eq!(lines[1], &['B' as u16]);
        assert_eq!(lines[2], &['C' as u16]);
        assert_eq!(lines[3], &['D' as u16]);
    }

    #[test]
    fn narrow_width_causes_word_wrapping_at_whitespace() {
        let text: Vec<u16> = "hello world foo bar".encode_utf16().collect();
        // Each char is 10 units. max_width is 60 units -> fits 6 chars ("hello ").
        let pages = paginate(&text, 60, 10, |s| Some(s.len() as i32 * 10)).unwrap();
        assert_eq!(pages.len(), 1);
        let line_strings: Vec<String> = pages[0]
            .lines
            .iter()
            .map(|l| String::from_utf16_lossy(l))
            .collect();
        assert_eq!(line_strings, vec!["hello", "world", "foo", "bar"]);
    }

    #[test]
    fn unbroken_token_hard_breaks_and_makes_forward_progress() {
        let text: Vec<u16> = "abcdefghijkl".encode_utf16().collect();
        // Each char 10 units, max_width 40 units -> fits 4 chars.
        let pages = paginate(&text, 40, 10, |s| Some(s.len() as i32 * 10)).unwrap();
        assert_eq!(pages.len(), 1);
        let line_strings: Vec<String> = pages[0]
            .lines
            .iter()
            .map(|l| String::from_utf16_lossy(l))
            .collect();
        assert_eq!(line_strings, vec!["abcd", "efgh", "ijkl"]);
    }

    #[test]
    fn surrogate_pairs_are_not_split() {
        // '𐐀' is U+10400 encoded as [0xD801, 0xDC00] (2 u16 code units)
        let text: Vec<u16> = "𐐀𐐀𐐀".encode_utf16().collect();
        assert_eq!(text.len(), 6);
        // Each code unit is 10 units. If max_width is 30, it could try to fit 3 code units.
        // It must not split the 2nd surrogate pair!
        let pages = paginate(&text, 30, 10, |s| Some(s.len() as i32 * 10)).unwrap();
        assert_eq!(pages.len(), 1);
        for line in &pages[0].lines {
            // Every line must contain an even number of code units (intact surrogate pairs)
            assert_eq!(line.len() % 2, 0);
            assert!(line.len() <= 2);
        }
    }

    #[test]
    fn tabs_are_expanded_to_eight_column_stops() {
        let text: Vec<u16> = "a\tb".encode_utf16().collect();
        let expanded = expand_tabs(&text);
        // 'a' is 1 col -> tab adds 7 spaces -> 'b' is at column 8. Total length = 9.
        assert_eq!(expanded.len(), 9);
        assert_eq!(String::from_utf16_lossy(&expanded), "a       b");
    }

    #[test]
    fn multi_page_pagination_splits_lines_across_pages() {
        let text: Vec<u16> = "1\n2\n3\n4\n5".encode_utf16().collect();
        // lines_per_page = 2 -> 3 pages: [1, 2], [3, 4], [5]
        let pages = paginate(&text, 100, 2, |s| Some(s.len() as i32 * 10)).unwrap();
        assert_eq!(pages.len(), 3);
        assert_eq!(pages[0].lines.len(), 2);
        assert_eq!(pages[1].lines.len(), 2);
        assert_eq!(pages[2].lines.len(), 1);
    }

    #[test]
    fn font_scaling_uses_printer_dpi_and_ignores_zoom() {
        // Logical 10pt font (100 tenths) at printer DPI 600
        let print_height_100_zoom = rendered_font_height(100, 600, 100);
        let print_height_200_zoom = rendered_font_height(100, 600, 100); // printer always uses 100% zoom
        let screen_height_200_zoom = rendered_font_height(100, 96, 200);

        assert_eq!(print_height_100_zoom, -83);
        assert_eq!(print_height_200_zoom, -83);
        assert_eq!(screen_height_200_zoom, -27);
    }

    #[test]
    fn split_logical_lines_preserves_trailing_empty_lines() {
        let text: Vec<u16> = "Line 1\nLine 2\n".encode_utf16().collect();
        let lines = split_logical_lines(&text);
        assert_eq!(lines.len(), 3);
        assert_eq!(
            lines[0],
            &[
                'L' as u16, 'i' as u16, 'n' as u16, 'e' as u16, ' ' as u16, '1' as u16
            ]
        );
        assert_eq!(
            lines[1],
            &[
                'L' as u16, 'i' as u16, 'n' as u16, 'e' as u16, ' ' as u16, '2' as u16
            ]
        );
        assert_eq!(lines[2], &[]);
    }

    #[test]
    fn wrap_line_skips_consecutive_whitespace_on_wrapped_lines() {
        let text: Vec<u16> = "first    second".encode_utf16().collect();
        // Each char is 10 units. max_width is 60 units -> fits "first "
        let wrapped = wrap_line(&text, 60, &|s| Some(s.len() as i32 * 10)).unwrap();
        assert_eq!(wrapped.len(), 2);
        assert_eq!(String::from_utf16_lossy(&wrapped[0]), "first");
        assert_eq!(String::from_utf16_lossy(&wrapped[1]), "second");
    }

    #[test]
    fn expand_tabs_handles_multiple_tabs_and_alignments() {
        let text: Vec<u16> = "1234567\tX\tY".encode_utf16().collect();
        let expanded = expand_tabs(&text);
        // "1234567" is 7 cols -> tab 1 adds 1 space -> "X" is at col 8 (length 9).
        // "X" is 1 col -> tab 2 adds 7 spaces -> "Y" is at col 16 (length 17).
        assert_eq!(String::from_utf16_lossy(&expanded), "1234567 X       Y");
    }

    #[test]
    fn measurement_failure_propagates_as_none() {
        let text: Vec<u16> = "Hello world".encode_utf16().collect();
        let result = paginate(&text, 100, 10, |_| None);
        assert_eq!(result, None);
    }
}
