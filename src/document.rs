//! Reading and writing documents in the exact form they were found on disk.
//!
//! A read returns an [`OpenedDocument`] carrying the [`DocumentFormat`] (charset, BOM,
//! line-break style) it detected, and [`save_atomic`] requires that format back. Writing
//! a file in a different form than it was read in is how a UTF-8-with-BOM file silently
//! loses its BOM and a LF file comes back from a one-line edit with every line changed —
//! so do not add a read or save path that drops it.

use std::fmt;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use windows::Win32::Storage::FileSystem::{
    MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH, MoveFileExW, REPLACEFILE_IGNORE_MERGE_ERRORS,
    ReplaceFileW,
};
use windows::core::HSTRING;

use crate::line_endings::{self, LineEnding};
use crate::log;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Charset {
    Utf8,
    Utf16Le,
    Utf16Be,
    Utf32Le,
    Utf32Be,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DocumentFormat {
    pub charset: Charset,
    pub has_bom: bool,
    /// Defaults to CRLF, which is what a document never read from disk is saved as.
    pub line_ending: LineEnding,
}

impl Default for DocumentFormat {
    fn default() -> Self {
        Self { charset: Charset::Utf8, has_bom: false, line_ending: LineEnding::Crlf }
    }
}

impl DocumentFormat {
    fn bom_len(&self) -> usize {
        if !self.has_bom {
            return 0;
        }
        match self.charset {
            Charset::Utf8 => 3,
            Charset::Utf16Le | Charset::Utf16Be => 2,
            Charset::Utf32Le | Charset::Utf32Be => 4,
        }
    }
}

/// Identifies the charset from a BOM, or `None` when the bytes carry none.
///
/// Order matters: the UTF-32 LE BOM (FF FE 00 00) begins with the UTF-16 LE BOM
/// (FF FE), so it must be tested first or every UTF-32 LE file is misread as UTF-16.
pub fn detect_bom(bytes: &[u8]) -> Option<Charset> {
    match bytes {
        [0xFF, 0xFE, 0x00, 0x00, ..] => Some(Charset::Utf32Le),
        [0x00, 0x00, 0xFE, 0xFF, ..] => Some(Charset::Utf32Be),
        [0xEF, 0xBB, 0xBF, ..] => Some(Charset::Utf8),
        [0xFF, 0xFE, ..] => Some(Charset::Utf16Le),
        [0xFE, 0xFF, ..] => Some(Charset::Utf16Be),
        _ => None,
    }
}

#[derive(Debug)]
pub enum ReadError {
    Io(io::Error),
    /// The bytes are not valid in the detected charset (realistically: a Shift-JIS file).
    InvalidEncoding,
}

impl fmt::Display for ReadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ReadError::Io(e) => write!(f, "{e}"),
            ReadError::InvalidEncoding => f.write_str(INVALID_ENCODING_MESSAGE),
        }
    }
}

pub const INVALID_ENCODING_MESSAGE: &str = "このファイルは UTF-8 として読み取れませんでした。Shift-JIS など、このアプリが対応していない文字コードの可能性があります。ファイルは変更していません。";

/// Strictly decodes `bytes`. Strictness is the point: substituting U+FFFD for
/// undecodable input is what destroys non-UTF-8 files, because saving then writes the
/// replacement characters over the original bytes irreversibly. Refusing to open
/// leaves the file untouched.
pub fn decode(bytes: &[u8]) -> Result<(String, DocumentFormat), ReadError> {
    let (charset, has_bom) = match detect_bom(bytes) {
        Some(c) => (c, true),
        None => (Charset::Utf8, false),
    };
    let mut format = DocumentFormat { charset, has_bom, line_ending: LineEnding::Crlf };
    let body = &bytes[format.bom_len()..];

    let text = match charset {
        Charset::Utf8 => String::from_utf8(body.to_vec()).map_err(|_| ReadError::InvalidEncoding)?,
        Charset::Utf16Le | Charset::Utf16Be => {
            if body.len() % 2 != 0 {
                return Err(ReadError::InvalidEncoding);
            }
            let units: Vec<u16> = body
                .chunks_exact(2)
                .map(|c| match charset {
                    Charset::Utf16Le => u16::from_le_bytes([c[0], c[1]]),
                    _ => u16::from_be_bytes([c[0], c[1]]),
                })
                .collect();
            String::from_utf16(&units).map_err(|_| ReadError::InvalidEncoding)?
        }
        Charset::Utf32Le | Charset::Utf32Be => {
            if body.len() % 4 != 0 {
                return Err(ReadError::InvalidEncoding);
            }
            body.chunks_exact(4)
                .map(|c| {
                    let b = [c[0], c[1], c[2], c[3]];
                    let v = match charset {
                        Charset::Utf32Le => u32::from_le_bytes(b),
                        _ => u32::from_be_bytes(b),
                    };
                    char::from_u32(v).ok_or(ReadError::InvalidEncoding)
                })
                .collect::<Result<String, _>>()?
        }
    };

    format.line_ending = line_endings::detect(&text);
    Ok((text, format))
}

/// Encodes `text` (whose line breaks must already be normalized) in `format`,
/// BOM included when the format had one.
pub fn encode(text: &str, format: &DocumentFormat) -> Vec<u8> {
    let mut out = Vec::with_capacity(text.len() + 4);
    match format.charset {
        Charset::Utf8 => {
            if format.has_bom {
                out.extend_from_slice(&[0xEF, 0xBB, 0xBF]);
            }
            out.extend_from_slice(text.as_bytes());
        }
        Charset::Utf16Le | Charset::Utf16Be => {
            let le = format.charset == Charset::Utf16Le;
            let iter = format.has_bom.then_some(0xFEFFu16).into_iter().chain(text.encode_utf16());
            for unit in iter {
                out.extend_from_slice(&if le { unit.to_le_bytes() } else { unit.to_be_bytes() });
            }
        }
        Charset::Utf32Le | Charset::Utf32Be => {
            let le = format.charset == Charset::Utf32Le;
            let iter = format.has_bom.then_some(0xFEFFu32).into_iter().chain(text.chars().map(u32::from));
            for v in iter {
                out.extend_from_slice(&if le { v.to_le_bytes() } else { v.to_be_bytes() });
            }
        }
    }
    out
}

pub struct OpenedDocument {
    pub path: PathBuf,
    pub content: String,
    pub format: DocumentFormat,
}

pub fn read(path: &Path) -> Result<OpenedDocument, ReadError> {
    let bytes = fs::read(path).map_err(ReadError::Io)?;
    let (content, format) = decode(&bytes).inspect_err(|e| {
        if matches!(e, ReadError::InvalidEncoding) {
            log::error(&format!("read({})", path.display()), "invalid encoding");
        }
    })?;
    Ok(OpenedDocument { path: path.to_path_buf(), content, format })
}

/// Writes `content` to `path` in `format` without ever leaving `path` half-written.
///
/// The bytes go to a sibling temp file first (a sibling, not %TEMP%, so the swap stays
/// on one volume) and are then swapped in. A disk-full, a dropped network drive or a
/// crash midway leaves the original intact rather than truncated.
pub fn save_atomic(path: &Path, content: &str, format: &DocumentFormat) -> io::Result<()> {
    let full = std::path::absolute(path)?;
    let dir = full
        .parent()
        .ok_or_else(|| io::Error::other(format!("保存先のフォルダを特定できませんでした: {}", path.display())))?;
    let file_name = full.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let temp = dir.join(format!("{file_name}.{:016x}.tmp", random_u64()));

    let bytes = encode(&line_endings::normalize(content, format.line_ending), format);
    let result = (|| {
        {
            let mut file = fs::File::create_new(&temp)?;
            file.write_all(&bytes)?;
            file.sync_all()?;
        }
        if full.exists() {
            // Preferred: ReplaceFile keeps the original's attributes and ACLs.
            let replaced = unsafe {
                ReplaceFileW(
                    &HSTRING::from(full.as_os_str()),
                    &HSTRING::from(temp.as_os_str()),
                    None,
                    REPLACEFILE_IGNORE_MERGE_ERRORS,
                    None,
                    None,
                )
            };
            if let Err(e) = replaced {
                // Not every file system supports ReplaceFile (notably some network
                // shares). MoveFileEx with REPLACE_EXISTING is still an atomic swap; it
                // just doesn't carry the original's attributes over.
                log::error(&format!("save_atomic({}) ReplaceFile fallback", full.display()), &e);
                move_file(&temp, &full, true)?;
            }
        } else {
            move_file(&temp, &full, false)?;
        }
        Ok(())
    })();

    // A successful swap consumed the temp file; anything else must not leave debris
    // next to the user's document. Never let cleanup replace the real outcome.
    if temp.exists() {
        if let Err(e) = fs::remove_file(&temp) {
            log::error(&format!("remove temp {}", temp.display()), e);
        }
    }
    result
}

fn move_file(from: &Path, to: &Path, overwrite: bool) -> io::Result<()> {
    let flags = if overwrite { MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH } else { MOVEFILE_WRITE_THROUGH };
    unsafe { MoveFileExW(&HSTRING::from(from.as_os_str()), &HSTRING::from(to.as_os_str()), flags) }
        .map_err(|e| io::Error::from_raw_os_error((e.code().0 & 0xFFFF) as i32))
}

fn random_u64() -> u64 {
    use std::hash::{BuildHasher, Hasher};
    let mut h = std::collections::hash_map::RandomState::new().build_hasher();
    h.write_u128(std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0));
    h.finish()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn utf8_bom_round_trips() {
        let bytes = b"\xEF\xBB\xBF# hi\r\nthere".to_vec();
        let (text, fmt) = decode(&bytes).unwrap();
        assert_eq!(text, "# hi\r\nthere");
        assert_eq!(fmt, DocumentFormat { charset: Charset::Utf8, has_bom: true, line_ending: LineEnding::Crlf });
        assert_eq!(encode(&text, &fmt), bytes);
    }

    #[test]
    fn utf16_and_utf32_round_trip() {
        for charset in [Charset::Utf16Le, Charset::Utf16Be, Charset::Utf32Le, Charset::Utf32Be] {
            let fmt = DocumentFormat { charset, has_bom: true, line_ending: LineEnding::Lf };
            let bytes = encode("日本語\n😀", &fmt);
            assert_eq!(detect_bom(&bytes), Some(charset));
            let (text, back) = decode(&bytes).unwrap();
            assert_eq!(text, "日本語\n😀");
            assert_eq!(back, fmt);
        }
    }

    #[test]
    fn rejects_shift_jis() {
        // "日本" in Shift-JIS
        assert!(matches!(decode(&[0x93, 0xFA, 0x96, 0x7B]), Err(ReadError::InvalidEncoding)));
    }

    #[test]
    fn save_keeps_line_endings_and_replaces_atomically() {
        let dir = std::env::temp_dir().join(format!("mymdedit-test-{:x}", random_u64()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("a.md");
        fs::write(&path, b"old\nfile\n").unwrap();
        let doc = read(&path).unwrap();
        assert_eq!(doc.format.line_ending, LineEnding::Lf);
        // The editor hands back lone \r breaks.
        save_atomic(&path, "new\rfile\r", &doc.format).unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"new\nfile\n");
        // No temp debris left behind.
        assert_eq!(fs::read_dir(&dir).unwrap().count(), 1);
        let new_path = dir.join("b.md");
        save_atomic(&new_path, "x", &DocumentFormat::default()).unwrap();
        assert_eq!(fs::read(&new_path).unwrap(), b"x");
        fs::remove_dir_all(&dir).unwrap();
    }
}
