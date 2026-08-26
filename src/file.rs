use std::fs;
use std::io;
use std::path::Path;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Encoding {
    Utf8,
    Utf8Bom,
    Utf16Le,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Newline {
    CrLf,
    Lf,
    Cr,
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
    pub text: String,
    pub format: TextFormat,
}

pub fn load(path: &Path) -> io::Result<LoadedText> {
    decode(&fs::read(path)?)
}

pub fn save(path: &Path, edit_text: &str, format: TextFormat) -> io::Result<()> {
    fs::write(path, encode(edit_text, format))
}

pub fn decode(bytes: &[u8]) -> io::Result<LoadedText> {
    decode_with_legacy(bytes, decode_ansi)
}

fn decode_with_legacy(
    bytes: &[u8],
    legacy_decoder: fn(&[u8]) -> io::Result<String>,
) -> io::Result<LoadedText> {
    let (text, encoding) = if bytes.starts_with(&[0xEF, 0xBB, 0xBF]) {
        (
            std::str::from_utf8(&bytes[3..])
                .map_err(invalid_utf8)?
                .to_owned(),
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
        let units: Vec<u16> = body
            .chunks_exact(2)
            .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
            .collect();
        (
            String::from_utf16(&units)
                .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error.to_string()))?,
            Encoding::Utf16Le,
        )
    } else if let Ok(text) = std::str::from_utf8(bytes) {
        (text.to_owned(), Encoding::Utf8)
    } else {
        // Classic Windows text commonly uses the active ANSI code page. Once
        // loaded, save it as UTF-8 so a lossy reverse conversion is impossible.
        (legacy_decoder(bytes)?, Encoding::Utf8)
    };

    let newline = detect_newline(&text);
    Ok(LoadedText {
        text: normalize_for_edit(&text),
        format: TextFormat { encoding, newline },
    })
}

pub fn encode(edit_text: &str, format: TextFormat) -> Vec<u8> {
    let text = apply_newline_style(edit_text, format.newline);
    match format.encoding {
        Encoding::Utf8 => text.into_bytes(),
        Encoding::Utf8Bom => {
            let mut bytes = Vec::with_capacity(text.len() + 3);
            bytes.extend_from_slice(&[0xEF, 0xBB, 0xBF]);
            bytes.extend_from_slice(text.as_bytes());
            bytes
        }
        Encoding::Utf16Le => {
            let mut bytes = Vec::with_capacity(text.len() * 2 + 2);
            bytes.extend_from_slice(&[0xFF, 0xFE]);
            for unit in text.encode_utf16() {
                bytes.extend_from_slice(&unit.to_le_bytes());
            }
            bytes
        }
    }
}

fn invalid_utf8(error: std::str::Utf8Error) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, error.to_string())
}

fn detect_newline(text: &str) -> Newline {
    let bytes = text.as_bytes();
    let mut crlf = 0;
    let mut lf = 0;
    let mut cr = 0;
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'\r' if bytes.get(index + 1) == Some(&b'\n') => {
                crlf += 1;
                index += 2;
            }
            b'\r' => {
                cr += 1;
                index += 1;
            }
            b'\n' => {
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

fn normalize_for_edit(text: &str) -> String {
    let mut result = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(ch) = chars.next() {
        match ch {
            '\r' => {
                if chars.peek() == Some(&'\n') {
                    chars.next();
                }
                result.push_str("\r\n");
            }
            '\n' => result.push_str("\r\n"),
            _ => result.push(ch),
        }
    }
    result
}

fn apply_newline_style(text: &str, newline: Newline) -> String {
    let separator = match newline {
        Newline::CrLf => "\r\n",
        Newline::Lf => "\n",
        Newline::Cr => "\r",
    };
    let mut result = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(ch) = chars.next() {
        match ch {
            '\r' => {
                if chars.peek() == Some(&'\n') {
                    chars.next();
                }
                result.push_str(separator);
            }
            '\n' => result.push_str(separator),
            _ => result.push(ch),
        }
    }
    result
}

#[cfg(windows)]
fn decode_ansi(bytes: &[u8]) -> io::Result<String> {
    use windows_sys::Win32::Globalization::{CP_ACP, MultiByteToWideChar};

    if bytes.is_empty() {
        return Ok(String::new());
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
    let mut wide = vec![0u16; wide_len as usize];
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
    String::from_utf16(&wide)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error.to_string()))
}

#[cfg(not(windows))]
fn decode_ansi(bytes: &[u8]) -> io::Result<String> {
    Ok(String::from_utf8_lossy(bytes).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_utf8_and_normalizes_lf_for_the_edit_control() {
        let loaded = decode("one\ntwo\n".as_bytes()).unwrap();
        assert_eq!(loaded.format.encoding, Encoding::Utf8);
        assert_eq!(loaded.format.newline, Newline::Lf);
        assert_eq!(loaded.text, "one\r\ntwo\r\n");
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
        assert_eq!(loaded.text, "A😀");
        assert_eq!(encode(&loaded.text, loaded.format), bytes);
    }

    #[test]
    fn rejects_truncated_utf16() {
        assert!(decode(&[0xFF, 0xFE, 0x41]).is_err());
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
        assert_eq!(loaded.text, "one\r\ntwo\r\n");
        assert_eq!(encode(&loaded.text, loaded.format), b"one\rtwo\r");
    }

    #[test]
    fn empty_input_round_trips_in_each_encoding() {
        let loaded = decode(&[]).unwrap();
        assert_eq!(loaded.text, "");
        assert_eq!(loaded.format, TextFormat::default());
        assert_eq!(encode("", loaded.format), b"");
        assert_eq!(
            encode(
                "",
                TextFormat {
                    encoding: Encoding::Utf8Bom,
                    newline: Newline::CrLf,
                },
            ),
            [0xEF, 0xBB, 0xBF]
        );
        assert_eq!(
            encode(
                "",
                TextFormat {
                    encoding: Encoding::Utf16Le,
                    newline: Newline::CrLf,
                },
            ),
            [0xFF, 0xFE]
        );
    }

    #[test]
    fn invalid_utf8_uses_the_legacy_decoder_and_promotes_to_utf8() {
        fn deterministic_legacy_decoder(bytes: &[u8]) -> io::Result<String> {
            assert_eq!(bytes, [0x80]);
            Ok("€".to_owned())
        }

        let loaded = decode_with_legacy(&[0x80], deterministic_legacy_decoder).unwrap();
        assert_eq!(loaded.text, "€");
        assert_eq!(loaded.format.encoding, Encoding::Utf8);
    }
}
