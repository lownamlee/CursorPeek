use std::{env, path::PathBuf};

const BINARY_NAME: &str = "CursorPeek";
const MANIFEST_PATH: &str = "assets/windows/CursorPeek.manifest";
const SUPPORTED_TARGET: &str = "x86_64-pc-windows-msvc";

fn main() {
    println!("cargo::rerun-if-changed={MANIFEST_PATH}");

    let target = env::var("TARGET").expect("Cargo always supplies TARGET to build scripts");
    assert_eq!(
        target, SUPPORTED_TARGET,
        "CursorPeek currently supports only {SUPPORTED_TARGET}"
    );

    let manifest = PathBuf::from(
        env::var_os("CARGO_MANIFEST_DIR")
            .expect("Cargo always supplies CARGO_MANIFEST_DIR to build scripts"),
    )
    .join(MANIFEST_PATH);

    println!("cargo::rustc-link-arg-bin={BINARY_NAME}=/MANIFEST:EMBED");
    println!(
        "cargo::rustc-link-arg-bin={BINARY_NAME}=/MANIFESTINPUT:{}",
        manifest.display()
    );
}
