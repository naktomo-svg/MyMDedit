#![no_main]
use libfuzzer_sys::fuzz_target;
use mymdedit_fuzz::document;

// Whatever a file holds: decoding never panics, and what it accepts is encoded back byte
// for byte — a file opened and saved unedited must not change on disk.
fuzz_target!(|data: &[u8]| {
    if let Ok((text, format)) = document::decode(data) {
        assert!(!text.contains('\0'));
        assert_eq!(document::encode(&text, &format), data, "round trip changed the bytes");
    }
});
