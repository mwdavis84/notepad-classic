use std::fs::{self, File};
use std::io::{self, BufWriter, Write};
use std::path::Path;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Encoding {
    Utf8,
    Utf8Bom,
    Utf16Le,
}

impl Encoding {
    pub const fn status_resource_id(self) -> usize {
        match self {
            Encoding::Utf8 => crate::localization::ids::IDS_STATUS_ENCODING_UTF8,
            Encoding::Utf8Bom => crate::localization::ids::IDS_STATUS_ENCODING_UTF8_BOM,
            Encoding::Utf16Le => crate::localization::ids::IDS_STATUS_ENCODING_UTF16_LE,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Newline {
    CrLf,
    Lf,
    Cr,
}

impl Newline {
    pub const fn status_resource_id(self) -> usize {
        match self {
            Newline::CrLf => crate::localization::ids::IDS_STATUS_EOL_CRLF,
            Newline::Lf => crate::localization::ids::IDS_STATUS_EOL_LF,
            Newline::Cr => crate::localization::ids::IDS_STATUS_EOL_CR,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TextFormat {
    pub encoding: Encoding,
    pub newline: Newline,
}

impl Default for TextFormat {
    fn default() -> Self {
        Self {
            encoding: Encoding::Utf8,
            newline: Newline::CrLf,
        }
    }
}

pub struct LoadedText {
    /// CRLF-normalized UTF-16, ready to pass to the native EDIT control.
    pub text: Vec<u16>,
    pub format: TextFormat,
}

pub fn load(path: &Path) -> io::Result<LoadedText> {
    decode(&fs::read(path)?)
}

pub fn save(path: &Path, edit_text: &[u16], format: TextFormat) -> io::Result<()> {
    let file = File::create(path)?;
    let mut writer = BufWriter::with_capacity(64 * 1024, file);
    encode_to_writer(&mut writer, edit_text, format)?;
    writer.flush()
}

pub fn decode(bytes: &[u8]) -> io::Result<LoadedText> {
    decode_with_legacy(bytes, decode_ansi)
}

fn decode_with_legacy(
    bytes: &[u8],
    legacy_decoder: fn(&[u8]) -> io::Result<Vec<u16>>,
) -> io::Result<LoadedText> {
    let (mut text, encoding) = if bytes.starts_with(&[0xEF, 0xBB, 0xBF]) {
        let text = std::str::from_utf8(&bytes[3..]).map_err(invalid_utf8)?;
        (
            collect_utf16_for_edit(text.encode_utf16()),
            Encoding::Utf8Bom,
        )
    } else if bytes.starts_with(&[0xFF, 0xFE]) {
        let body = &bytes[2..];
        if body.len() % 2 != 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "UTF-16 LE file has an incomplete code unit",
            ));
        }
        let units = collect_utf16_for_edit(
            body.chunks_exact(2)
                .map(|pair| u16::from_le_bytes([pair[0], pair[1]])),
        );
        validate_utf16(&units)?;
        (units, Encoding::Utf16Le)
    } else if let Ok(text) = std::str::from_utf8(bytes) {
        (collect_utf16_for_edit(text.encode_utf16()), Encoding::Utf8)
    } else {
        // Classic Windows text commonly uses the active ANSI code page. Once
        // loaded, save it as UTF-8 so a lossy reverse conversion is impossible.
        (legacy_decoder(bytes)?, Encoding::Utf8)
    };

    let newline = detect_newline(&text);
    normalize_for_edit(&mut text);
    Ok(LoadedText {
        text,
        format: TextFormat { encoding, newline },
    })
}

fn collect_utf16_for_edit<I>(units: I) -> Vec<u16>
where
    I: Iterator<Item = u16> + Clone,
{
    let unit_count = units.clone().count();
    let extra = normalization_growth(units.clone());
    let mut text = Vec::with_capacity(unit_count + extra + 1);
    text.extend(units);
    text
}

fn normalization_growth(units: impl Iterator<Item = u16>) -> usize {
    let mut extra = 0;
    let mut previous_was_cr = false;
    for unit in units {
        if previous_was_cr {
            if unit == b'\n' as u16 {
                previous_was_cr = false;
                continue;
            }
            extra += 1;
            previous_was_cr = false;
        }
        if unit == b'\r' as u16 {
            previous_was_cr = true;
        } else if unit == b'\n' as u16 {
            extra += 1;
        }
    }
    extra + usize::from(previous_was_cr)
}

fn encode_to_writer(
    writer: &mut impl Write,
    edit_text: &[u16],
    format: TextFormat,
) -> io::Result<()> {
    write_bom(writer, format.encoding)?;
    encode_body_to_writer(writer, edit_text, format)
}

fn write_bom(writer: &mut impl Write, encoding: Encoding) -> io::Result<()> {
    match encoding {
        Encoding::Utf8 => Ok(()),
        Encoding::Utf8Bom => writer.write_all(&[0xEF, 0xBB, 0xBF]),
        Encoding::Utf16Le => writer.write_all(&[0xFF, 0xFE]),
    }
}

fn encode_body_to_writer(
    writer: &mut impl Write,
    edit_text: &[u16],
    format: TextFormat,
) -> io::Result<()> {
    match format.encoding {
        Encoding::Utf8 | Encoding::Utf8Bom => encode_utf8_body(writer, edit_text, format.newline),
        Encoding::Utf16Le => encode_utf16_le_body(writer, edit_text, format.newline),
    }
}

fn encode_utf8_body(
    writer: &mut impl Write,
    edit_text: &[u16],
    newline: Newline,
) -> io::Result<()> {
    let mut output = EncodeBuffer::new(writer);
    let mut index = 0;
    while index < edit_text.len() {
        let unit = edit_text[index];
        if unit == b'\r' as u16 || unit == b'\n' as u16 {
            if unit == b'\r' as u16 && edit_text.get(index + 1) == Some(&(b'\n' as u16)) {
                index += 1;
            }
            for &separator_unit in newline_units(newline) {
                output.push_byte(separator_unit as u8)?;
            }
        } else {
            let decoded = if is_high_surrogate(unit)
                && edit_text
                    .get(index + 1)
                    .is_some_and(|next| is_low_surrogate(*next))
            {
                let high = u32::from(unit) - 0xD800;
                let low = u32::from(edit_text[index + 1]) - 0xDC00;
                index += 1;
                char::from_u32(0x10000 + (high << 10) + low).unwrap()
            } else if is_high_surrogate(unit) || is_low_surrogate(unit) {
                char::REPLACEMENT_CHARACTER
            } else {
                char::from_u32(u32::from(unit)).unwrap()
            };
            output.push_char(decoded)?;
        }
        index += 1;
    }
    output.finish()
}

fn encode_utf16_le_body(
    writer: &mut impl Write,
    edit_text: &[u16],
    newline: Newline,
) -> io::Result<()> {
    let mut output = EncodeBuffer::new(writer);
    let mut index = 0;
    while index < edit_text.len() {
        let unit = edit_text[index];
        if unit == b'\r' as u16 || unit == b'\n' as u16 {
            if unit == b'\r' as u16 && edit_text.get(index + 1) == Some(&(b'\n' as u16)) {
                index += 1;
            }
            for &separator_unit in newline_units(newline) {
                output.push_u16_le(separator_unit)?;
            }
        } else if is_high_surrogate(unit)
            && edit_text
                .get(index + 1)
                .is_some_and(|next| is_low_surrogate(*next))
        {
            output.push_u16_le(unit)?;
            index += 1;
            output.push_u16_le(edit_text[index])?;
        } else if is_high_surrogate(unit) || is_low_surrogate(unit) {
            output.push_u16_le(char::REPLACEMENT_CHARACTER as u16)?;
        } else {
            output.push_u16_le(unit)?;
        }
        index += 1;
    }
    output.finish()
}

const ENCODE_BUFFER_SIZE: usize = 16 * 1024;

struct EncodeBuffer<'a, W: Write> {
    writer: &'a mut W,
    bytes: [u8; ENCODE_BUFFER_SIZE],
    len: usize,
}

impl<'a, W: Write> EncodeBuffer<'a, W> {
    fn new(writer: &'a mut W) -> Self {
        Self {
            writer,
            bytes: [0; ENCODE_BUFFER_SIZE],
            len: 0,
        }
    }

    fn push_byte(&mut self, byte: u8) -> io::Result<()> {
        if self.len == self.bytes.len() {
            self.flush()?;
        }
        self.bytes[self.len] = byte;
        self.len += 1;
        Ok(())
    }

    fn push_char(&mut self, ch: char) -> io::Result<()> {
        let mut encoded = [0u8; 4];
        self.push_bytes(ch.encode_utf8(&mut encoded).as_bytes())
    }

    fn push_u16_le(&mut self, unit: u16) -> io::Result<()> {
        self.push_bytes(&unit.to_le_bytes())
    }

    fn push_bytes(&mut self, bytes: &[u8]) -> io::Result<()> {
        if self.bytes.len() - self.len < bytes.len() {
            self.flush()?;
        }
        let end = self.len + bytes.len();
        self.bytes[self.len..end].copy_from_slice(bytes);
        self.len = end;
        Ok(())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.writer.write_all(&self.bytes[..self.len])?;
        self.len = 0;
        Ok(())
    }

    fn finish(mut self) -> io::Result<()> {
        self.flush()
    }
}

const fn newline_units(newline: Newline) -> &'static [u16] {
    match newline {
        Newline::CrLf => &[b'\r' as u16, b'\n' as u16],
        Newline::Lf => &[b'\n' as u16],
        Newline::Cr => &[b'\r' as u16],
    }
}

#[cfg(test)]
pub fn encode(edit_text: &[u16], format: TextFormat) -> Vec<u8> {
    let mut bytes = Vec::new();
    encode_to_writer(&mut bytes, edit_text, format).unwrap();
    bytes
}

fn invalid_utf8(error: std::str::Utf8Error) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, error.to_string())
}

fn detect_newline(text: &[u16]) -> Newline {
    let mut crlf = 0;
    let mut lf = 0;
    let mut cr = 0;
    let mut index = 0;
    while index < text.len() {
        match text[index] {
            unit if unit == b'\r' as u16 && text.get(index + 1) == Some(&(b'\n' as u16)) => {
                crlf += 1;
                index += 2;
            }
            unit if unit == b'\r' as u16 => {
                cr += 1;
                index += 1;
            }
            unit if unit == b'\n' as u16 => {
                lf += 1;
                index += 1;
            }
            _ => index += 1,
        }
    }
    if lf > crlf && lf >= cr {
        Newline::Lf
    } else if cr > crlf && cr > lf {
        Newline::Cr
    } else {
        Newline::CrLf
    }
}

fn normalize_for_edit(text: &mut Vec<u16>) {
    let extra = normalization_growth(text.iter().copied());
    // Preserve one spare code unit for WM_SETTEXT's trailing NUL. Decoders
    // reserve this up front; the legacy fallback grows only once, exactly as
    // much as normalization requires.
    text.reserve_exact(extra + 1);
    if extra == 0 {
        return;
    }

    let old_len = text.len();
    text.resize(old_len + extra, 0);
    let mut read = old_len;
    let mut write = text.len();
    while read > 0 {
        let unit = text[read - 1];
        if unit == b'\n' as u16 && read >= 2 && text[read - 2] == b'\r' as u16 {
            write -= 2;
            text[write] = b'\r' as u16;
            text[write + 1] = b'\n' as u16;
            read -= 2;
        } else if unit == b'\r' as u16 || unit == b'\n' as u16 {
            write -= 2;
            text[write] = b'\r' as u16;
            text[write + 1] = b'\n' as u16;
            read -= 1;
        } else {
            write -= 1;
            text[write] = unit;
            read -= 1;
        }
    }
    debug_assert_eq!(write, 0);
}

fn validate_utf16(text: &[u16]) -> io::Result<()> {
    if let Some(error) = char::decode_utf16(text.iter().copied()).find_map(Result::err) {
        Err(io::Error::new(
            io::ErrorKind::InvalidData,
            error.to_string(),
        ))
    } else {
        Ok(())
    }
}

const fn is_high_surrogate(unit: u16) -> bool {
    unit >= 0xD800 && unit <= 0xDBFF
}

const fn is_low_surrogate(unit: u16) -> bool {
    unit >= 0xDC00 && unit <= 0xDFFF
}

#[cfg(windows)]
fn decode_ansi(bytes: &[u8]) -> io::Result<Vec<u16>> {
    use windows_sys::Win32::Globalization::{CP_ACP, MultiByteToWideChar};

    if bytes.is_empty() {
        return Ok(Vec::new());
    }
    let input_len = i32::try_from(bytes.len())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "file is too large"))?;
    // SAFETY: `bytes` remains valid for both calls and the second call receives
    // exactly the buffer length returned by the first.
    let wide_len = unsafe {
        MultiByteToWideChar(
            CP_ACP,
            0,
            bytes.as_ptr(),
            input_len,
            std::ptr::null_mut(),
            0,
        )
    };
    if wide_len == 0 {
        return Err(io::Error::last_os_error());
    }
    let mut wide = Vec::with_capacity(wide_len as usize + 1);
    wide.resize(wide_len as usize, 0);
    let written = unsafe {
        MultiByteToWideChar(
            CP_ACP,
            0,
            bytes.as_ptr(),
            input_len,
            wide.as_mut_ptr(),
            wide_len,
        )
    };
    if written == 0 {
        return Err(io::Error::last_os_error());
    }
    validate_utf16(&wide)?;
    Ok(wide)
}

#[cfg(not(windows))]
fn decode_ansi(bytes: &[u8]) -> io::Result<Vec<u16>> {
    let text = String::from_utf8_lossy(bytes);
    Ok(collect_utf16_for_edit(text.encode_utf16()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static NEXT_TEMP_FILE: AtomicUsize = AtomicUsize::new(0);

    fn wide(text: &str) -> Vec<u16> {
        text.encode_utf16().collect()
    }

    fn temp_file(name: &str) -> std::path::PathBuf {
        let sequence = NEXT_TEMP_FILE.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!(
            "notepad-classic-{name}-{}-{sequence}.tmp",
            std::process::id()
        ))
    }

    #[test]
    fn detects_utf8_and_normalizes_lf_for_the_edit_control() {
        let loaded = decode("one\ntwo\n".as_bytes()).unwrap();
        assert_eq!(loaded.format.encoding, Encoding::Utf8);
        assert_eq!(loaded.format.newline, Newline::Lf);
        assert_eq!(loaded.text, wide("one\r\ntwo\r\n"));
        assert!(loaded.text.capacity() > loaded.text.len());
    }

    #[test]
    fn preserves_utf8_bom() {
        let loaded = decode(&[0xEF, 0xBB, 0xBF, b'x']).unwrap();
        assert_eq!(loaded.format.encoding, Encoding::Utf8Bom);
        assert_eq!(
            encode(&loaded.text, loaded.format),
            [0xEF, 0xBB, 0xBF, b'x']
        );
    }

    #[test]
    fn loads_and_saves_utf16_le() {
        let bytes = [0xFF, 0xFE, b'A', 0, 0x3D, 0xD8, 0x00, 0xDE];
        let loaded = decode(&bytes).unwrap();
        assert_eq!(loaded.format.encoding, Encoding::Utf16Le);
        assert_eq!(loaded.text, wide("A😀"));
        assert_eq!(encode(&loaded.text, loaded.format), bytes);
    }

    #[test]
    fn rejects_truncated_utf16() {
        assert!(decode(&[0xFF, 0xFE, 0x41]).is_err());
    }

    #[test]
    fn rejects_unpaired_utf16_surrogates() {
        assert!(decode(&[0xFF, 0xFE, 0x00, 0xD8]).is_err());
    }

    #[test]
    fn preserves_mixed_text_using_dominant_newline() {
        let loaded = decode(b"a\nb\nc\r\nd").unwrap();
        assert_eq!(loaded.format.newline, Newline::Lf);
        assert_eq!(encode(&loaded.text, loaded.format), b"a\nb\nc\nd");
    }

    #[test]
    fn preserves_classic_mac_carriage_return_newlines() {
        let loaded = decode(b"one\rtwo\r").unwrap();
        assert_eq!(loaded.format.newline, Newline::Cr);
        assert_eq!(loaded.text, wide("one\r\ntwo\r\n"));
        assert_eq!(encode(&loaded.text, loaded.format), b"one\rtwo\r");
    }

    #[test]
    fn empty_input_round_trips_in_each_encoding() {
        let loaded = decode(&[]).unwrap();
        assert!(loaded.text.is_empty());
        assert_eq!(loaded.format, TextFormat::default());
        assert_eq!(encode(&[], loaded.format), b"");
        assert_eq!(
            encode(
                &[],
                TextFormat {
                    encoding: Encoding::Utf8Bom,
                    newline: Newline::CrLf,
                },
            ),
            [0xEF, 0xBB, 0xBF]
        );
        assert_eq!(
            encode(
                &[],
                TextFormat {
                    encoding: Encoding::Utf16Le,
                    newline: Newline::CrLf,
                },
            ),
            [0xFF, 0xFE]
        );
    }

    #[test]
    fn empty_files_save_with_the_requested_bom() {
        let cases = [
            (Encoding::Utf8, Vec::new()),
            (Encoding::Utf8Bom, vec![0xEF, 0xBB, 0xBF]),
            (Encoding::Utf16Le, vec![0xFF, 0xFE]),
        ];
        for (encoding, expected) in cases {
            let path = temp_file("empty-save");
            save(
                &path,
                &[],
                TextFormat {
                    encoding,
                    newline: Newline::CrLf,
                },
            )
            .unwrap();
            assert_eq!(fs::read(&path).unwrap(), expected);
            fs::remove_file(path).unwrap();
        }
    }

    #[test]
    fn saves_non_ascii_and_supplementary_characters_in_each_encoding() {
        let text = wide("café 😀");
        let utf8 = "café 😀".as_bytes();
        assert_eq!(
            encode(
                &text,
                TextFormat {
                    encoding: Encoding::Utf8,
                    newline: Newline::CrLf,
                }
            ),
            utf8
        );

        let mut utf8_bom = vec![0xEF, 0xBB, 0xBF];
        utf8_bom.extend_from_slice(utf8);
        assert_eq!(
            encode(
                &text,
                TextFormat {
                    encoding: Encoding::Utf8Bom,
                    newline: Newline::CrLf,
                }
            ),
            utf8_bom
        );

        let mut utf16_le = vec![0xFF, 0xFE];
        for unit in text {
            utf16_le.extend_from_slice(&unit.to_le_bytes());
        }
        assert_eq!(
            encode(
                &wide("café 😀"),
                TextFormat {
                    encoding: Encoding::Utf16Le,
                    newline: Newline::CrLf,
                }
            ),
            utf16_le
        );
    }

    #[test]
    fn save_converts_mixed_input_to_each_newline_mode() {
        let text = wide("a\r\nb\nc\rd");
        for (newline, expected) in [
            (Newline::CrLf, "a\r\nb\r\nc\r\nd"),
            (Newline::Lf, "a\nb\nc\nd"),
            (Newline::Cr, "a\rb\rc\rd"),
        ] {
            assert_eq!(
                encode(
                    &text,
                    TextFormat {
                        encoding: Encoding::Utf8,
                        newline,
                    }
                ),
                expected.as_bytes()
            );
        }
    }

    #[test]
    fn invalid_utf8_uses_the_legacy_decoder_and_promotes_to_utf8() {
        fn deterministic_legacy_decoder(bytes: &[u8]) -> io::Result<Vec<u16>> {
            assert_eq!(bytes, [0x80]);
            Ok(wide("€"))
        }

        let loaded = decode_with_legacy(&[0x80], deterministic_legacy_decoder).unwrap();
        assert_eq!(loaded.text, wide("€"));
        assert_eq!(loaded.format.encoding, Encoding::Utf8);
    }

    #[test]
    fn maps_document_format_state_to_status_resource_ids() {
        use crate::localization::ids::*;

        assert_eq!(TextFormat::default().newline, Newline::CrLf);
        assert_eq!(TextFormat::default().encoding, Encoding::Utf8);
        assert_eq!(Newline::CrLf.status_resource_id(), IDS_STATUS_EOL_CRLF);
        assert_eq!(Newline::Lf.status_resource_id(), IDS_STATUS_EOL_LF);
        assert_eq!(Newline::Cr.status_resource_id(), IDS_STATUS_EOL_CR);
        assert_eq!(
            Encoding::Utf8.status_resource_id(),
            IDS_STATUS_ENCODING_UTF8
        );
        assert_eq!(
            Encoding::Utf8Bom.status_resource_id(),
            IDS_STATUS_ENCODING_UTF8_BOM
        );
        assert_eq!(
            Encoding::Utf16Le.status_resource_id(),
            IDS_STATUS_ENCODING_UTF16_LE
        );
    }

    #[test]
    fn legacy_ansi_mapping_promotes_to_utf8_status_resource_id() {
        use crate::localization::ids::*;

        fn deterministic_legacy_decoder(bytes: &[u8]) -> io::Result<Vec<u16>> {
            assert_eq!(bytes, [0x80]);
            Ok(wide("€"))
        }

        let loaded = decode_with_legacy(&[0x80], deterministic_legacy_decoder).unwrap();
        assert_eq!(loaded.format.encoding, Encoding::Utf8);
        assert_eq!(
            loaded.format.encoding.status_resource_id(),
            IDS_STATUS_ENCODING_UTF8
        );
    }

    #[test]
    fn save_replaces_unpaired_surrogates_like_the_previous_lossy_string_path() {
        let text = [b'A' as u16, 0xD800, b'B' as u16];
        assert_eq!(encode(&text, TextFormat::default()), "A�B".as_bytes());
    }
}
