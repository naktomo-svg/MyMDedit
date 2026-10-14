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
use std::os::windows::io::AsRawHandle;
use std::path::{Path, PathBuf};

use windows::Win32::Foundation::HANDLE;
use windows::Win32::Storage::FileSystem::{
    FILE_TYPE_DISK, GetFileType, MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH, MoveFileExW,
    REPLACEFILE_IGNORE_MERGE_ERRORS, ReplaceFileW,
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
        Self {
            charset: Charset::Utf8,
            has_bom: false,
            line_ending: LineEnding::Crlf,
        }
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
    /// The bytes are not valid in the detected charset (for BOM-less input, realistically
    /// a Shift-JIS file; for a BOM'd UTF-16/32 file, a damaged one).
    InvalidEncoding(Charset),
    /// Decodes fine but contains U+0000 (realistically: BOM-less UTF-16). Every Win32
    /// text API the editor uses stops at the first NUL, so the rest would be silently
    /// dropped from view — and then from the file on the next save.
    ContainsNul,
}

impl Charset {
    fn label(self) -> &'static str {
        match self {
            Charset::Utf8 => "UTF-8",
            Charset::Utf16Le | Charset::Utf16Be => "UTF-16",
            Charset::Utf32Le | Charset::Utf32Be => "UTF-32",
        }
    }
}

impl fmt::Display for ReadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ReadError::Io(e) => write!(f, "{e}"),
            ReadError::InvalidEncoding(Charset::Utf8) => f.write_str(
                "このファイルは UTF-8 として読み取れませんでした。Shift-JIS など、このアプリが対応していない文字コードの可能性があります。ファイルは変更していません。",
            ),
            ReadError::InvalidEncoding(c) => write!(
                f,
                "このファイルは {} として読み取れませんでした。ファイルが壊れている可能性があります。ファイルは変更していません。",
                c.label()
            ),
            ReadError::ContainsNul => f.write_str(CONTAINS_NUL_MESSAGE),
        }
    }
}

/// The editor's UTF-16 as a String. A lone surrogate (pasteable, already corrupt) cannot
/// be represented in any charset this app writes, so it becomes U+FFFD — but the caller
/// is told, so the substitution is never silent.
pub fn text_from_editor(units: &[u16]) -> (String, bool) {
    match String::from_utf16(units) {
        Ok(s) => (s, false),
        Err(_) => (String::from_utf16_lossy(units), true),
    }
}

pub const CONTAINS_NUL_MESSAGE: &str = "このファイルには NUL 文字が含まれているため開けません。BOM のない UTF-16 など、このアプリが対応していない形式の可能性があります。ファイルは変更していません。";

/// Strictly decodes `bytes`. Strictness is the point: substituting U+FFFD for
/// undecodable input is what destroys non-UTF-8 files, because saving then writes the
/// replacement characters over the original bytes irreversibly. Refusing to open
/// leaves the file untouched.
pub fn decode(bytes: &[u8]) -> Result<(String, DocumentFormat), ReadError> {
    let (charset, has_bom) = match detect_bom(bytes) {
        Some(c) => (c, true),
        None => (Charset::Utf8, false),
    };
    let mut format = DocumentFormat {
        charset,
        has_bom,
        line_ending: LineEnding::Crlf,
    };
    let body = &bytes[format.bom_len()..];

    let invalid = || ReadError::InvalidEncoding(charset);
    let text = match charset {
        Charset::Utf8 => String::from_utf8(body.to_vec()).map_err(|_| invalid())?,
        Charset::Utf16Le | Charset::Utf16Be => {
            let (chunks, rest) = body.as_chunks::<2>();
            if !rest.is_empty() {
                return Err(invalid());
            }
            let units: Vec<u16> = chunks
                .iter()
                .map(|&c| {
                    if charset == Charset::Utf16Le {
                        u16::from_le_bytes(c)
                    } else {
                        u16::from_be_bytes(c)
                    }
                })
                .collect();
            String::from_utf16(&units).map_err(|_| invalid())?
        }
        Charset::Utf32Le | Charset::Utf32Be => {
            let (chunks, rest) = body.as_chunks::<4>();
            if !rest.is_empty() {
                return Err(invalid());
            }
            chunks
                .iter()
                .map(|&c| {
                    let v = if charset == Charset::Utf32Le {
                        u32::from_le_bytes(c)
                    } else {
                        u32::from_be_bytes(c)
                    };
                    char::from_u32(v).ok_or_else(invalid)
                })
                .collect::<Result<String, _>>()?
        }
    };

    // Checked after decoding, so it covers every charset — UTF-16/32 decode NUL too.
    if text.contains('\0') {
        return Err(ReadError::ContainsNul);
    }
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
            let iter = format
                .has_bom
                .then_some(0xFEFFu16)
                .into_iter()
                .chain(text.encode_utf16());
            for unit in iter {
                out.extend_from_slice(&if le {
                    unit.to_le_bytes()
                } else {
                    unit.to_be_bytes()
                });
            }
        }
        Charset::Utf32Le | Charset::Utf32Be => {
            let le = format.charset == Charset::Utf32Le;
            let iter = format
                .has_bom
                .then_some(0xFEFFu32)
                .into_iter()
                .chain(text.chars().map(u32::from));
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
    /// How the file looked just before it was read; `None` if that could not be told.
    pub stamp: Option<FileStamp>,
}

/// The largest document `read` accepts. Far past anything the editor handles well (and
/// the preview stops at ~2MB anyway), but small enough that reading, decoding and the
/// editor's UTF-16 copy cannot exhaust memory — an allocation failure aborts the
/// process, with no panic and no crash log.
pub const MAX_DOCUMENT_BYTES: u64 = 64 * 1024 * 1024;

/// Reads the whole file, or fails with `FileTooLarge` past `max` bytes. Counts what is
/// actually read rather than trusting the file's reported length, which can grow while
/// it is read (or be meaningless, as for a device or a pipe).
pub fn read_at_most(path: &Path, max: u64) -> io::Result<Vec<u8>> {
    use std::io::Read;
    let file = fs::File::open(path)?;
    // Checked on the opened handle, not the path: CreateFile happily opens CON, NUL, COM
    // ports and (remote) named pipes, and reading one can block until its writer goes away.
    if unsafe { GetFileType(HANDLE(file.as_raw_handle())) } != FILE_TYPE_DISK {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "通常のファイルではないため開けません（デバイスやパイプは開けません）。",
        ));
    }
    let mut bytes = Vec::new();
    file.take(max + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > max {
        return Err(io::Error::new(
            io::ErrorKind::FileTooLarge,
            format!(
                "ファイルが大きすぎるため開けません（上限は {} MB です）。ファイルは変更していません。",
                max / (1024 * 1024)
            ),
        ));
    }
    Ok(bytes)
}

/// What a file looked like on disk at one moment, to notice another program changing it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FileStamp {
    modified: std::time::SystemTime,
    len: u64,
}

pub fn stamp(path: &Path) -> io::Result<FileStamp> {
    let meta = fs::metadata(path)?;
    Ok(FileStamp {
        modified: meta.modified()?,
        len: meta.len(),
    })
}

#[derive(Debug, PartialEq, Eq)]
pub enum DiskChange {
    Unchanged,
    /// Differs from the recorded stamp; holds the new one.
    Modified(FileStamp),
    Missing,
}

/// Compares the file with `recorded`. A file that cannot be inspected for any reason but
/// being gone (a dropped network drive, a sharing violation) counts as unchanged: there
/// is nothing the user could do about it, and asking again on every activation would nag.
pub fn check_disk(path: &Path, recorded: FileStamp) -> DiskChange {
    match stamp(path) {
        Ok(now) if now == recorded => DiskChange::Unchanged,
        Ok(now) => DiskChange::Modified(now),
        Err(e) if e.kind() == io::ErrorKind::NotFound => DiskChange::Missing,
        Err(e) => {
            log::error(&format!("check_disk({})", path.display()), &e);
            DiskChange::Unchanged
        }
    }
}

pub fn read(path: &Path) -> Result<OpenedDocument, ReadError> {
    // Taken before reading: a change that lands mid-read then still shows up later as a
    // difference, rather than being folded into the stamp unseen.
    let stamp = stamp(path).ok();
    let bytes = read_at_most(path, MAX_DOCUMENT_BYTES).map_err(ReadError::Io)?;
    let (content, format) = decode(&bytes).inspect_err(|e| {
        if !matches!(e, ReadError::Io(_)) {
            log::error(&format!("read({})", path.display()), e);
        }
    })?;
    Ok(OpenedDocument {
        path: path.to_path_buf(),
        content,
        format,
        stamp,
    })
}

/// Writes `content` to `path` in `format` without ever leaving `path` half-written.
///
/// The bytes go to a sibling temp file first (a sibling, not %TEMP%, so the swap stays
/// on one volume) and are then swapped in. A disk-full, a dropped network drive or a
/// crash midway leaves the original intact rather than truncated.
pub fn save_atomic(path: &Path, content: &str, format: &DocumentFormat) -> io::Result<()> {
    let full = std::path::absolute(path)?;
    let dir = full.parent().ok_or_else(|| {
        io::Error::other(format!(
            "保存先のフォルダを特定できませんでした: {}",
            path.display()
        ))
    })?;
    let file_name = full
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let temp = dir.join(format!("{file_name}.{:016x}.tmp", random_u64()));

    let backup = dir.join(format!("{file_name}.{:016x}.bak", random_u64()));

    let bytes = encode(
        &line_endings::normalize(content, format.line_ending),
        format,
    );
    let existed = full.exists();
    let result = (|| {
        {
            let mut file = fs::File::create_new(&temp)?;
            file.write_all(&bytes)?;
            file.sync_all()?;
        }
        if existed {
            swap_into_place(&full, &temp, &backup, &SYSTEM_FILE_OPS)
        } else {
            move_file(&temp, &full, false)
        }
    })();

    match &result {
        // The target was there before and is gone now: the temp file is the only copy of
        // the document on disk, so it must survive, and the user must learn where it is.
        Err(e) if existed && !full.exists() && temp.exists() => {
            return Err(io::Error::new(
                e.kind(),
                format!(
                    "{e}（保存しようとした内容は {} に残しています）",
                    temp.display()
                ),
            ));
        }
        // Otherwise the target holds either the original or the new content, so a
        // leftover temp file is debris. Never let cleanup replace the real outcome.
        _ => remove_if_exists(&temp),
    }
    result
}

/// The file-system calls `swap_into_place` makes, injectable so its recovery paths can
/// be exercised by tests without provoking real ReplaceFile failures.
struct FileOps {
    replace: fn(target: &Path, replacement: &Path, backup: &Path) -> io::Result<()>,
    rename: fn(from: &Path, to: &Path, overwrite: bool) -> io::Result<()>,
}

const SYSTEM_FILE_OPS: FileOps = FileOps {
    replace: replace_file,
    rename: move_file,
};

/// Replaces existing `full` with `temp`, keeping the original recoverable at every step.
///
/// ReplaceFile can fail after it has already moved the original aside
/// (ERROR_UNABLE_TO_MOVE_REPLACEMENT_2). Without a backup name the original then sits
/// under a name nobody knows — and if the fallback also fails, the target is simply
/// gone. Passing `backup` makes that location known, so it can be put back.
fn swap_into_place(full: &Path, temp: &Path, backup: &Path, ops: &FileOps) -> io::Result<()> {
    // Preferred: ReplaceFile keeps the original's attributes and ACLs.
    let replaced = (ops.replace)(full, temp, backup);
    if let Err(e) = replaced {
        log::error(
            &format!("save_atomic({}) ReplaceFile failed", full.display()),
            &e,
        );
        if !full.exists() && backup.exists() {
            // Restore the original before trying anything else.
            (ops.rename)(backup, full, false).map_err(|restore| {
                io::Error::new(
                    restore.kind(),
                    format!(
                        "{e}（元のファイルは {} に退避されています）",
                        backup.display()
                    ),
                )
            })?;
        }
        // Not every file system supports ReplaceFile (notably some network shares).
        // MoveFileEx with REPLACE_EXISTING is still an atomic swap; it just doesn't
        // carry the original's attributes over.
        (ops.rename)(temp, full, true)?;
    }
    // The original's content has been superseded by a successful swap.
    remove_if_exists(backup);
    Ok(())
}

fn remove_if_exists(path: &Path) {
    if path.exists()
        && let Err(e) = fs::remove_file(path)
    {
        log::error(&format!("remove {}", path.display()), e);
    }
}

fn replace_file(target: &Path, replacement: &Path, backup: &Path) -> io::Result<()> {
    unsafe {
        ReplaceFileW(
            &HSTRING::from(target.as_os_str()),
            &HSTRING::from(replacement.as_os_str()),
            &HSTRING::from(backup.as_os_str()),
            REPLACEFILE_IGNORE_MERGE_ERRORS,
            None,
            None,
        )
    }
    .map_err(|e| io::Error::from_raw_os_error(e.code().0 & 0xFFFF))
}

fn move_file(from: &Path, to: &Path, overwrite: bool) -> io::Result<()> {
    let flags = if overwrite {
        MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH
    } else {
        MOVEFILE_WRITE_THROUGH
    };
    unsafe {
        MoveFileExW(
            &HSTRING::from(from.as_os_str()),
            &HSTRING::from(to.as_os_str()),
            flags,
        )
    }
    .map_err(|e| io::Error::from_raw_os_error(e.code().0 & 0xFFFF))
}

fn random_u64() -> u64 {
    use std::hash::{BuildHasher, Hasher};
    let mut h = std::collections::hash_map::RandomState::new().build_hasher();
    h.write_u128(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0),
    );
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
        assert_eq!(
            fmt,
            DocumentFormat {
                charset: Charset::Utf8,
                has_bom: true,
                line_ending: LineEnding::Crlf
            }
        );
        assert_eq!(encode(&text, &fmt), bytes);
    }

    #[test]
    fn utf16_and_utf32_round_trip() {
        for charset in [
            Charset::Utf16Le,
            Charset::Utf16Be,
            Charset::Utf32Le,
            Charset::Utf32Be,
        ] {
            let fmt = DocumentFormat {
                charset,
                has_bom: true,
                line_ending: LineEnding::Lf,
            };
            let bytes = encode("日本語\n😀", &fmt);
            assert_eq!(detect_bom(&bytes), Some(charset));
            let (text, back) = decode(&bytes).unwrap();
            assert_eq!(text, "日本語\n😀");
            assert_eq!(back, fmt);
        }
    }

    fn temp_dir() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("mymdedit-test-{:x}", random_u64()));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// ERROR_UNABLE_TO_MOVE_REPLACEMENT_2: the original was moved aside, the swap did not happen.
    fn replace_moves_original_then_fails(target: &Path, _: &Path, backup: &Path) -> io::Result<()> {
        fs::rename(target, backup)?;
        Err(io::Error::from_raw_os_error(1177))
    }

    fn replace_unsupported(_: &Path, _: &Path, _: &Path) -> io::Result<()> {
        Err(io::Error::from_raw_os_error(50))
    }

    /// A rename that is blocked (sharing violation) whenever it would move the temp file.
    fn rename_refusing_temp(from: &Path, to: &Path, overwrite: bool) -> io::Result<()> {
        if from.extension().is_some_and(|e| e == "tmp") {
            return Err(io::Error::from_raw_os_error(32));
        }
        move_file(from, to, overwrite)
    }

    #[test]
    fn partial_replace_failure_restores_the_original() {
        let dir = temp_dir();
        let (full, temp, backup) = (
            dir.join("a.md"),
            dir.join("a.md.1.tmp"),
            dir.join("a.md.1.bak"),
        );
        fs::write(&full, "original").unwrap();
        fs::write(&temp, "new").unwrap();
        let ops = FileOps {
            replace: replace_moves_original_then_fails,
            rename: rename_refusing_temp,
        };
        assert!(swap_into_place(&full, &temp, &backup, &ops).is_err());
        assert_eq!(fs::read_to_string(&full).unwrap(), "original");
        assert!(!backup.exists());
        assert_eq!(fs::read_to_string(&temp).unwrap(), "new");
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn unsupported_replace_falls_back_to_move() {
        let dir = temp_dir();
        let (full, temp, backup) = (
            dir.join("a.md"),
            dir.join("a.md.1.tmp"),
            dir.join("a.md.1.bak"),
        );
        fs::write(&full, "original").unwrap();
        fs::write(&temp, "new").unwrap();
        let ops = FileOps {
            replace: replace_unsupported,
            rename: move_file,
        };
        swap_into_place(&full, &temp, &backup, &ops).unwrap();
        assert_eq!(fs::read_to_string(&full).unwrap(), "new");
        assert_eq!(fs::read_dir(&dir).unwrap().count(), 1);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn disk_check_sees_unchanged_modified_and_missing() {
        let dir = temp_dir();
        let path = dir.join("a.md");
        fs::write(&path, "one").unwrap();
        let recorded = stamp(&path).unwrap();
        assert_eq!(check_disk(&path, recorded), DiskChange::Unchanged);
        fs::write(&path, "three").unwrap();
        let DiskChange::Modified(now) = check_disk(&path, recorded) else {
            panic!("not seen as modified")
        };
        assert_eq!(check_disk(&path, now), DiskChange::Unchanged);
        fs::remove_file(&path).unwrap();
        assert_eq!(check_disk(&path, now), DiskChange::Missing);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn reads_are_capped() {
        let dir = temp_dir();
        let path = dir.join("big.md");
        fs::write(&path, vec![b'a'; 11]).unwrap();
        assert_eq!(read_at_most(&path, 11).unwrap().len(), 11);
        let e = read_at_most(&path, 10).unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::FileTooLarge);
        assert!(e.to_string().contains("大きすぎる"));
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn devices_are_refused() {
        // A device or pipe can block the read forever (CON waits for the keyboard, a
        // pipe for its writer) — on the UI thread, before the window even appears.
        let e = read_at_most(Path::new("NUL"), 10).unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::InvalidInput);
        assert!(e.to_string().contains("通常のファイルではない"));
    }

    #[test]
    fn rejects_text_containing_nul() {
        // BOM-less UTF-16LE "ab": valid UTF-8 byte-wise, but the editor would cut it at the NUL.
        assert!(decode(b"a\0b\0").is_err());
        assert!(decode(b"x\0y").is_err());
    }

    #[test]
    fn rejects_shift_jis() {
        // "日本" in Shift-JIS
        assert!(matches!(
            decode(&[0x93, 0xFA, 0x96, 0x7B]),
            Err(ReadError::InvalidEncoding(_))
        ));
    }

    #[test]
    fn encoding_errors_name_the_charset() {
        let bad_utf16 = [0xFF, 0xFE, 0x00, 0xD8]; // BOM + lone high surrogate
        let e = decode(&bad_utf16).unwrap_err();
        assert!(matches!(e, ReadError::InvalidEncoding(Charset::Utf16Le)));
        assert!(e.to_string().contains("UTF-16"));
        assert!(
            decode(&[0x93, 0xFA])
                .unwrap_err()
                .to_string()
                .contains("UTF-8")
        );
    }

    #[test]
    fn editor_text_with_lone_surrogate_is_flagged() {
        assert_eq!(text_from_editor(&[0x61, 0x62]), ("ab".to_string(), false));
        let (s, replaced) = text_from_editor(&[0x61, 0xD800]);
        assert_eq!(s, "a\u{FFFD}");
        assert!(replaced);
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
