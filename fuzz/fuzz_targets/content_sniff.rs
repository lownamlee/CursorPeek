#![no_main]

use cursorpeek_core::harness::exercise_content_sniff;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| exercise_content_sniff(data));
