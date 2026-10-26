#![no_main]
libfuzzer_sys::fuzz_target!(|data: &[u8]| kborg::fuzzing::secret_masked(data));
