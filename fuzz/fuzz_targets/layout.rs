#![no_main]

use cursorpeek_core::harness::exercise_layout;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| exercise_layout(data));
