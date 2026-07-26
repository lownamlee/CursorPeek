use std::env;

const RESOURCE_INPUTS: &[&str] = &[
    "assets/windows/CursorPeek.ico",
    "assets/windows/CursorPeek.manifest",
];
const SUPPORTED_TARGET: &str = "x86_64-pc-windows-msvc";

fn main() {
    for input in RESOURCE_INPUTS {
        println!("cargo::rerun-if-changed={input}");
    }
    println!("cargo::rerun-if-env-changed=CARGO_PKG_VERSION");

    let target = env::var("TARGET").expect("Cargo always supplies TARGET to build scripts");
    assert_eq!(
        target, SUPPORTED_TARGET,
        "CursorPeek currently supports only {SUPPORTED_TARGET}"
    );

    let package_version =
        env::var("CARGO_PKG_VERSION").expect("Cargo always supplies CARGO_PKG_VERSION");
    let file_version = format!(
        "{}.{}.{}.0",
        env::var("CARGO_PKG_VERSION_MAJOR")
            .expect("Cargo always supplies the package major version"),
        env::var("CARGO_PKG_VERSION_MINOR")
            .expect("Cargo always supplies the package minor version"),
        env::var("CARGO_PKG_VERSION_PATCH")
            .expect("Cargo always supplies the package patch version"),
    );

    winresource::WindowsResource::new()
        .set_icon_with_id("assets/windows/CursorPeek.ico", "101")
        .set_manifest_file("assets/windows/CursorPeek.manifest")
        .set_language(0x0409)
        .set("CompanyName", "CursorPeek contributors")
        .set("FileDescription", "File Explorer hover previews")
        .set("FileVersion", &file_version)
        .set("InternalName", "CursorPeek")
        .set("OriginalFilename", "CursorPeek.exe")
        .set("ProductName", "CursorPeek")
        .set("ProductVersion", &package_version)
        .compile()
        .expect("CursorPeek Windows resources should compile and link");
}
