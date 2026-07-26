use std::{fs, path::Path};

use cursorpeek_core::{
    harness::{exercise_content_sniff, exercise_layout, exercise_payload, exercise_protocol},
    layout::MAX_PREVIEW_PAYLOAD_LEN,
};

const CORPUS_ROOT: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/fuzz/corpus");

#[test]
fn retained_fuzz_corpus_replays_on_the_stable_windows_build() {
    replay("protocol", MAX_PREVIEW_PAYLOAD_LEN + 24, exercise_protocol);
    replay("payload", MAX_PREVIEW_PAYLOAD_LEN, exercise_payload);
    replay("content_sniff", 64 * 1024 + 1, exercise_content_sniff);
    replay("layout", 16, exercise_layout);
}

fn replay(target: &str, maximum_bytes: usize, exercise: fn(&[u8])) {
    let directory = Path::new(CORPUS_ROOT).join(target);
    let mut files = fs::read_dir(&directory)
        .unwrap_or_else(|error| panic!("read fuzz corpus `{}`: {error}", directory.display()))
        .map(|entry| {
            entry
                .unwrap_or_else(|error| {
                    panic!("enumerate fuzz corpus `{}`: {error}", directory.display())
                })
                .path()
        })
        .filter(|path| path.is_file())
        .collect::<Vec<_>>();
    files.sort();
    assert!(
        !files.is_empty(),
        "fuzz target `{target}` must retain at least one regression seed"
    );

    for path in files {
        replay_one(target, maximum_bytes, &path, exercise);
    }
}

fn replay_one(target: &str, maximum_bytes: usize, path: &Path, exercise: fn(&[u8])) {
    let bytes = fs::read(path)
        .unwrap_or_else(|error| panic!("read retained fuzz input `{}`: {error}", path.display()));
    assert!(
        bytes.len() <= maximum_bytes,
        "retained `{target}` input `{}` is {} bytes; the harness cap is {maximum_bytes}",
        path.display(),
        bytes.len()
    );
    exercise(&bytes);
}
