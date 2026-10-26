#![no_main]
libfuzzer_sys::fuzz_target!(|data: &[u8]| kborg::fuzzing::archive_extract(data));
