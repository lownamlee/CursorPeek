use super::{VectorDecodeResult, decode};
use crate::worker::{file::PreviewFile, payload::VectorPreview};
use cursorpeek_core::svg::MAX_VECTOR_SOURCE_BYTES;
use std::{
    fs, io,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

const MANIFEST: &str = include_str!("../../../corpus/vector/cases.tsv");
static NEXT_CORPUS_DIRECTORY: AtomicU64 = AtomicU64::new(1);

#[derive(Clone, Copy)]
enum Generator {
    Document(&'static str),
    Utf16Document(&'static str),
    Oversized,
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum Expected {
    Still,
    Animated,
    Fallback(&'static str),
}

struct CorpusCase {
    id: &'static str,
    generator: Generator,
    expected: Expected,
    render: bool,
}

pub(crate) struct RenderCorpusCase {
    pub(crate) id: &'static str,
    pub(crate) preview: VectorPreview,
}

#[test]
fn generated_corpus_manifest_matches_every_executable_case() {
    let manifest_ids = MANIFEST
        .lines()
        .skip(1)
        .filter(|line| !line.trim().is_empty())
        .map(|line| {
            line.split_once('\t')
                .expect("each vector corpus row must contain a tab")
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
fn generated_vector_corpus_obeys_the_render_and_refusal_contract() {
    let root = TestDirectory::new("contract");
    for case in corpus_cases() {
        let expects_preview = matches!(case.expected, Expected::Still | Expected::Animated);
        assert_eq!(
            run_case(&root, &case).is_some(),
            expects_preview,
            "corpus case `{}` outcome kind",
            case.id
        );
    }
}

pub(crate) fn renderable_previews() -> Vec<RenderCorpusCase> {
    let root = TestDirectory::new("render");
    corpus_cases()
        .into_iter()
        .filter(|case| case.render)
        .map(|case| RenderCorpusCase {
            id: case.id,
            preview: run_case(&root, &case)
                .expect("render corpus cases must produce vector frames"),
        })
        .collect()
}

fn run_case(root: &TestDirectory, case: &CorpusCase) -> Option<VectorPreview> {
    let path = root.path().join(format!("{}.svg", case.id));
    match case.generator {
        Generator::Document(source) => {
            fs::write(&path, source.as_bytes()).expect("the corpus document should be written");
        }
        Generator::Utf16Document(source) => {
            let mut bytes = vec![0xff, 0xfe];
            for unit in source.encode_utf16() {
                bytes.extend_from_slice(&unit.to_le_bytes());
            }
            fs::write(&path, bytes).expect("the UTF-16 corpus document should be written");
        }
        Generator::Oversized => {
            let file =
                fs::File::create(&path).expect("the oversized corpus file should be created");
            file.set_len(MAX_VECTOR_SOURCE_BYTES + 1)
                .expect("the oversized corpus file should be extended");
        }
    }

    let file = PreviewFile::open(&path).expect("the corpus document should open");
    let decoded = decode(&file).expect("corpus decoding should not fail the file layer");
    match (case.expected, decoded) {
        (Expected::Fallback(reason), VectorDecodeResult::Fallback(actual)) => {
            assert_eq!(actual, reason, "corpus case `{}` refusal reason", case.id);
            None
        }
        (Expected::Still | Expected::Animated, VectorDecodeResult::Fallback(actual)) => {
            panic!("corpus case `{}` should render but was refused: {actual}", case.id)
        }
        (Expected::Fallback(_), VectorDecodeResult::Preview(_)) => {
            panic!("corpus case `{}` should be refused", case.id)
        }
        (expected, VectorDecodeResult::Preview(preview)) => {
            let animated = expected == Expected::Animated;
            assert_eq!(preview.animated, animated, "corpus case `{}` animation", case.id);
            assert_eq!(
                preview.frames.len() > 1,
                animated,
                "corpus case `{}` frame count",
                case.id
            );
            assert_eq!(
                preview.frame_delay_ms > 0,
                animated,
                "corpus case `{}` frame delay",
                case.id
            );
            let frame_bytes = (preview.width * preview.height * 4) as usize;
            for frame in &preview.frames {
                assert_eq!(frame.len(), frame_bytes, "corpus case `{}` frame size", case.id);
            }
            assert!(
                preview.frames.iter().any(|frame| frame.iter().any(|byte| *byte != 0)),
                "corpus case `{}` must paint something",
                case.id
            );
            Some(preview)
        }
    }
}

fn corpus_cases() -> Vec<CorpusCase> {
    vec![
        still(
            "static-shapes",
            "<svg xmlns='http://www.w3.org/2000/svg' viewBox='0 0 24 24'>\
             <rect width='24' height='24' fill='#f0f0f0'/>\
             <path d='M4 20 L12 4 L20 20 Z' fill='#2f81f7' stroke='#0b3d91' stroke-width='1.5'/>\
             <line x1='4' y1='22' x2='20' y2='22' stroke='black' stroke-linecap='round'/></svg>",
            true,
        ),
        still(
            "rounded-and-ellipse",
            "<svg viewBox='0 0 40 20'><rect x='1' y='1' width='18' height='18' rx='4' ry='4' \
             fill='teal'/><ellipse cx='30' cy='10' rx='8' ry='6' fill='orange' \
             transform='rotate(15 30 10)'/></svg>",
            false,
        ),
        still(
            "polyline-and-fill-rule",
            "<svg viewBox='0 0 20 20'><polygon points='10,1 4,19 19,7 1,7 16,19' \
             fill='purple' fill-rule='evenodd'/><polyline points='1,1 19,1 19,3' \
             fill='none' stroke='black'/></svg>",
            false,
        ),
        still(
            "gradient-linear-bounding-box",
            "<svg viewBox='0 0 32 16'><defs><linearGradient id='g'>\
             <stop offset='0' stop-color='#ff0000'/><stop offset='1' stop-color='#0000ff'/>\
             </linearGradient></defs><rect width='32' height='16' fill='url(#g)'/></svg>",
            true,
        ),
        still(
            "gradient-radial-user-space",
            "<svg viewBox='0 0 20 20'><defs><radialGradient id='r' gradientUnits='userSpaceOnUse' \
             cx='10' cy='10' r='9' spreadMethod='reflect'><stop offset='0' stop-color='white'/>\
             <stop offset='1' stop-color='#204080' stop-opacity='0.8'/></radialGradient></defs>\
             <circle cx='10' cy='10' r='9' fill='url(#r)'/></svg>",
            false,
        ),
        still(
            "gradient-href-inheritance",
            "<svg viewBox='0 0 20 10'><defs><linearGradient id='base'>\
             <stop offset='0' stop-color='black'/><stop offset='1' stop-color='white'/>\
             </linearGradient><linearGradient id='tilted' href='#base' \
             gradientTransform='rotate(45)'/></defs>\
             <rect width='20' height='10' fill='url(#tilted)'/></svg>",
            false,
        ),
        still(
            "css-class-and-inline-style",
            "<svg viewBox='0 0 20 10'><style>/* c */ .fill { fill: red } \
             #one { stroke: navy; stroke-width: 2 } @media print { rect { fill: pink } }</style>\
             <rect id='one' class='fill' width='20' height='10' style='fill-opacity:0.75'/></svg>",
            false,
        ),
        still(
            "use-and-symbol",
            "<svg viewBox='0 0 30 10'><defs><symbol id='dot'><circle cx='5' cy='5' r='4' \
             fill='green'/></symbol></defs><use href='#dot'/><use href='#dot' x='10'/>\
             <use href='#dot' x='20' opacity='0.5'/></svg>",
            false,
        ),
        still(
            "nested-viewport-slice",
            "<svg viewBox='0 0 20 20'><svg x='2' y='2' width='16' height='16' \
             viewBox='0 0 8 4' preserveAspectRatio='xMinYMin slice'>\
             <rect width='8' height='4' fill='crimson'/></svg></svg>",
            false,
        ),
        still(
            "dashed-stroke",
            "<svg viewBox='0 0 40 8'><path d='M2 4 H38' stroke='black' stroke-width='3' \
             stroke-dasharray='6 3' stroke-dashoffset='2' fill='none'/></svg>",
            false,
        ),
        animated(
            "smil-attribute-animation",
            "<svg viewBox='0 0 40 10'><rect width='10' height='10' fill='#2f81f7'>\
             <animate attributeName='x' from='0' to='30' dur='900ms' \
             repeatCount='indefinite'/></rect></svg>",
            true,
        ),
        animated(
            "smil-transform-animation",
            "<svg viewBox='0 0 20 20'><g><animateTransform attributeName='transform' \
             type='rotate' values='0;360' dur='1s' repeatCount='indefinite'/>\
             <rect x='8' y='2' width='4' height='8' fill='black'/></g></svg>",
            false,
        ),
        animated(
            "smil-set-freeze",
            "<svg viewBox='0 0 10 10'><rect width='10' height='10' fill='gray'>\
             <set attributeName='fill' to='red' begin='400ms'/></rect></svg>",
            false,
        ),
        fallback(
            "script-element",
            "<svg viewBox='0 0 10 10'><script>fetch('https://example.test')</script>\
             <rect width='10' height='10'/></svg>",
            "active_content",
        ),
        fallback(
            "event-handler-attribute",
            "<svg viewBox='0 0 10 10'><rect width='10' height='10' onload='alert(1)'/></svg>",
            "active_content",
        ),
        fallback(
            "embedded-image-element",
            "<svg viewBox='0 0 10 10'><image href='#local'/></svg>",
            "active_content",
        ),
        fallback(
            "external-paint-reference",
            "<svg viewBox='0 0 10 10'><rect width='10' height='10' \
             fill='url(https://example.test/g.svg#g)'/></svg>",
            "external_reference",
        ),
        fallback(
            "entity-declaration",
            "<!DOCTYPE svg [<!ENTITY secret SYSTEM 'file:///c:/secret'>]>\
             <svg viewBox='0 0 10 10'><rect width='10' height='10'/></svg>",
            "entity_declaration",
        ),
        fallback(
            "malformed-markup",
            "<svg viewBox='0 0 10 10'><rect width='10' height='10'",
            "malformed_markup",
        ),
        fallback(
            "malformed-path-data",
            "<svg viewBox='0 0 10 10'><path d='M0 0 Q'/></svg>",
            "malformed_path",
        ),
        fallback(
            "non-svg-root",
            "<html><body>not a drawing</body></html>",
            "not_svg",
        ),
        fallback(
            "text-only-document",
            "<svg viewBox='0 0 20 10'><text x='1' y='8'>preview</text></svg>",
            "nothing_painted",
        ),
        CorpusCase {
            id: "utf16-document",
            generator: Generator::Utf16Document(
                "<svg viewBox='0 0 10 10'><rect width='10' height='10'/></svg>",
            ),
            expected: Expected::Fallback("not_utf8"),
            render: false,
        },
        CorpusCase {
            id: "oversized-document",
            generator: Generator::Oversized,
            expected: Expected::Fallback("file_too_large"),
            render: false,
        },
        fallback(
            "recursive-use-chain",
            "<svg viewBox='0 0 10 10'><use id='a' href='#b'/><use id='b' href='#a'/></svg>",
            "too_complex",
        ),
    ]
}

fn still(id: &'static str, source: &'static str, render: bool) -> CorpusCase {
    CorpusCase {
        id,
        generator: Generator::Document(source),
        expected: Expected::Still,
        render,
    }
}

fn animated(id: &'static str, source: &'static str, render: bool) -> CorpusCase {
    CorpusCase {
        id,
        generator: Generator::Document(source),
        expected: Expected::Animated,
        render,
    }
}

fn fallback(id: &'static str, source: &'static str, reason: &'static str) -> CorpusCase {
    CorpusCase {
        id,
        generator: Generator::Document(source),
        expected: Expected::Fallback(reason),
        render: false,
    }
}

struct TestDirectory {
    path: PathBuf,
}

impl TestDirectory {
    fn new(label: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "cursorpeek-vector-corpus-{label}-{}-{}",
            std::process::id(),
            NEXT_CORPUS_DIRECTORY.fetch_add(1, Ordering::Relaxed)
        ));
        match fs::create_dir(&path) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => panic!("create the vector corpus directory: {error}"),
        }
        Self { path }
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TestDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}
