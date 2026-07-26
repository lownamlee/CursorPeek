#![no_main]

use cursorpeek_core::harness::exercise_protocol;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| exercise_protocol(data));
