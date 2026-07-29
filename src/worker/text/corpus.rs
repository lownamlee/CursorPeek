use super::{TextDecodeResult, decode};
use crate::{
    preview_file::PreviewFile,
    settings::LegacyEncoding,
    worker::payload::{MAX_TEXT_LINES, MAX_TEXT_SCALARS, TextPreview},
};
use encoding_rs::{SHIFT_JIS, WINDOWS_1252};
use std::{
    env, fs, io,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

static NEXT_CORPUS_DIRECTORY: AtomicU64 = AtomicU64::new(1);
const MANIFEST: &str = include_str!("../../../corpus/text/cases.tsv");

struct CorpusCase {
    id: &'static str,
    bytes: Vec<u8>,
    policy: LegacyEncoding,
    expected: Expected,
}

enum Expected {
    Preview {
        encoding: &'static str,
        text: String,
        guessed: bool,
        truncated: bool,
    },
    Unsupported,
}

#[test]
fn generated_corpus_manifest_matches_every_executable_case() {
    let manifest_ids = MANIFEST
        .lines()
        .skip(1)
        .map(|line| {
            line.split_once('\t')
                .expect("each corpus manifest row must contain a tab")
                .0
        })
        .collect::<Vec<_>>();
    let executable_ids = corpus_cases()
        .into_iter()
        .map(|case| case.id)
        .collect::<Vec<_>>();

    assert_eq!(manifest_ids, executable_ids);
}

#[test]
fn generated_multilingual_and_hostile_corpus_obeys_the_product_contract() {
    let root = TestDirectory::new();

    for case in corpus_cases() {
        let path = root.path().join(format!("{}.txt", case.id));
        fs::write(&path, &case.bytes).expect("the generated corpus case should be written");
        let file = PreviewFile::open(&path).expect("the generated corpus case should open");
        let expected_linked_content = file.is_linked_content();
        let expected_last_write_time = file.last_write_time();
        let expected_display_name = file.display_name();
        let actual = decode(&file, &case.policy).expect("the corpus decode should not perform I/O");

        match case.expected {
            Expected::Preview {
                encoding,
                text,
                guessed,
                truncated,
            } => assert_eq!(
                actual,
                TextDecodeResult::Preview(TextPreview {
                    file_size: case.bytes.len() as u64,
                    last_write_time: expected_last_write_time,
                    linked_content: expected_linked_content,
                    encoding_was_guessed: guessed,
                    truncated,
                    display_name: expected_display_name,
                    encoding: encoding.to_owned(),
                    text,
                }),
                "corpus case `{}` did not produce its canonical preview",
                case.id
            ),
            Expected::Unsupported => assert_eq!(
                actual,
                TextDecodeResult::Unsupported,
                "corpus case `{}` must fail closed",
                case.id
            ),
        }
    }
}

fn corpus_cases() -> Vec<CorpusCase> {
    let multiscript = concat!(
        "Latin café\n",
        "Ελληνικά\n",
        "Русский\n",
        "العربية\n",
        "עברית\n",
        "हिन्दी\n",
        "ไทย\n",
        "中文\n",
        "日本語\n",
        "한국어\n",
        "emoji 👩🏽‍💻\n",
        "combining e\u{301}"
    );
    let combining_emoji = "Z͑̾a̓͗l̽g̿o̐\nfamily 👨‍👩‍👧‍👦\nflags 🇲🇾 🇸🇬";
    let utf16le_source = "alpha\r\nβeta\r漢字\u{0085}emoji 😀";
    let utf16be_source = "العربية\nעברית\nไทย";
    let utf32le_source = "supplementary: 𐐷 𝄞 😀";
    let utf32be_source = "right-to-left: العربية — עברית";
    let windows_1252_source = "“Résumé”—café costs €5.";
    let shift_jis_source = "日本語のテキストです。";
    let hostile_source =
        "safe prefix before escape \u{001b}[31mred\nbidi marker \u{202e}neutralized";
    let hostile_expected =
        "safe prefix before escape \u{fffd}[31mred\nbidi marker \u{fffd}neutralized";
    let svg_markup = concat!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n",
        "<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 16 16\">\n",
        "  <title>badge · 徽章</title>\n",
        "  <script>alert(1)</script>\n",
        "  <image href=\"https://example.invalid/pixel.png\"/>\n",
        "  <rect width=\"16\" height=\"16\" onload=\"alert(2)\"/>\n",
        "</svg>\n"
    );
    let line_limit_source = (0..=MAX_TEXT_LINES)
        .map(|line| format!("line-{line}"))
        .collect::<Vec<_>>()
        .join("\n");
    let line_limit_expected = (0..MAX_TEXT_LINES)
        .map(|line| format!("line-{line}"))
        .collect::<Vec<_>>()
        .join("\n");
    let scalar_limit_source = "x".repeat(MAX_TEXT_SCALARS + 1);
    let scalar_limit_expected = "x".repeat(MAX_TEXT_SCALARS);

    let (windows_1252, _, windows_1252_errors) = WINDOWS_1252.encode(windows_1252_source);
    assert!(!windows_1252_errors);
    let (shift_jis, _, shift_jis_errors) = SHIFT_JIS.encode(shift_jis_source);
    assert!(!shift_jis_errors);

    vec![
        preview(
            "utf8-multiscript",
            multiscript.as_bytes().to_vec(),
            LegacyEncoding::Auto,
            "UTF-8",
            multiscript,
            false,
            false,
        ),
        preview(
            "utf8-combining-emoji",
            combining_emoji.as_bytes().to_vec(),
            LegacyEncoding::Auto,
            "UTF-8",
            combining_emoji,
            false,
            false,
        ),
        preview(
            "utf16le-mixed-endings",
            encode_utf16(utf16le_source, Endian::Little),
            LegacyEncoding::Auto,
            "UTF-16 LE",
            "alpha\nβeta\n漢字\nemoji 😀",
            false,
            false,
        ),
        preview(
            "utf16be-multiscript",
            encode_utf16(utf16be_source, Endian::Big),
            LegacyEncoding::Auto,
            "UTF-16 BE",
            utf16be_source,
            false,
            false,
        ),
        preview(
            "utf32le-supplementary",
            encode_utf32(utf32le_source, Endian::Little),
            LegacyEncoding::Auto,
            "UTF-32 LE",
            utf32le_source,
            false,
            false,
        ),
        preview(
            "utf32be-rtl",
            encode_utf32(utf32be_source, Endian::Big),
            LegacyEncoding::Auto,
            "UTF-32 BE",
            utf32be_source,
            false,
            false,
        ),
        preview(
            "windows1252-explicit",
            windows_1252.into_owned(),
            LegacyEncoding::Label("windows-1252".to_owned()),
            "windows-1252",
            windows_1252_source,
            false,
            false,
        ),
        preview(
            "shiftjis-explicit",
            shift_jis.into_owned(),
            LegacyEncoding::Label("shift_jis".to_owned()),
            "Shift_JIS",
            shift_jis_source,
            false,
            false,
        ),
        preview(
            "controls-bidi-sanitized",
            hostile_source.as_bytes().to_vec(),
            LegacyEncoding::Auto,
            "UTF-8",
            hostile_expected,
            false,
            false,
        ),
        // SVG markup is retained verbatim: no element, attribute, or external reference in it is
        // parsed, resolved, or executed on the way to the preview.
        preview(
            "svg-inert-markup",
            svg_markup.as_bytes().to_vec(),
            LegacyEncoding::Auto,
            "UTF-8",
            svg_markup,
            false,
            false,
        ),
        preview(
            "line-limit",
            line_limit_source.into_bytes(),
            LegacyEncoding::Auto,
            "UTF-8",
            &line_limit_expected,
            false,
            true,
        ),
        preview(
            "scalar-limit",
            scalar_limit_source.into_bytes(),
            LegacyEncoding::Auto,
            "UTF-8",
            &scalar_limit_expected,
            false,
            true,
        ),
        unsupported(
            "malformed-utf8-bom",
            b"\xef\xbb\xbf\xf0\x28\x8c\x28".to_vec(),
        ),
        unsupported("malformed-utf16-bom", b"\xff\xfe\x3d\xd8A\x00".to_vec()),
        unsupported("png-masquerade", b"\x89PNG\r\n\x1a\nnot text".to_vec()),
        unsupported(
            "control-heavy",
            b"\x01\x02\x03\x04\x05\x06\x07\x08".to_vec(),
        ),
    ]
}

fn preview(
    id: &'static str,
    bytes: Vec<u8>,
    policy: LegacyEncoding,
    encoding: &'static str,
    text: &str,
    guessed: bool,
    truncated: bool,
) -> CorpusCase {
    CorpusCase {
        id,
        bytes,
        policy,
        expected: Expected::Preview {
            encoding,
            text: text.to_owned(),
            guessed,
            truncated,
        },
    }
}

fn unsupported(id: &'static str, bytes: Vec<u8>) -> CorpusCase {
    CorpusCase {
        id,
        bytes,
        policy: LegacyEncoding::Auto,
        expected: Expected::Unsupported,
    }
}

#[derive(Clone, Copy)]
enum Endian {
    Little,
    Big,
}

fn encode_utf16(text: &str, endian: Endian) -> Vec<u8> {
    let mut bytes = match endian {
        Endian::Little => vec![0xff, 0xfe],
        Endian::Big => vec![0xfe, 0xff],
    };
    for unit in text.encode_utf16() {
        bytes.extend(match endian {
            Endian::Little => unit.to_le_bytes(),
            Endian::Big => unit.to_be_bytes(),
        });
    }
    bytes
}

fn encode_utf32(text: &str, endian: Endian) -> Vec<u8> {
    let mut bytes = match endian {
        Endian::Little => vec![0xff, 0xfe, 0x00, 0x00],
        Endian::Big => vec![0x00, 0x00, 0xfe, 0xff],
    };
    for scalar in text.chars().map(u32::from) {
        bytes.extend(match endian {
            Endian::Little => scalar.to_le_bytes(),
            Endian::Big => scalar.to_be_bytes(),
        });
    }
    bytes
}

struct TestDirectory(PathBuf);

impl TestDirectory {
    fn new() -> Self {
        for _ in 0..32 {
            let sequence = NEXT_CORPUS_DIRECTORY.fetch_add(1, Ordering::Relaxed);
            let path = env::temp_dir().join(format!(
                "cursorpeek-text-corpus-{}-{sequence}",
                std::process::id()
            ));
            match fs::create_dir(&path) {
                Ok(()) => return Self(path),
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
                Err(error) => panic!("test directory `{}` failed: {error}", path.display()),
            }
        }
        panic!("could not reserve a unique text-corpus directory");
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TestDirectory {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0)
            .unwrap_or_else(|error| panic!("test cleanup `{}` failed: {error}", self.0.display()));
    }
}
