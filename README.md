# Windows CursorPeek

Windows CursorPeek is a lightweight native Windows utility for quickly previewing image and text
files by hovering over them in File Explorer.

It is written in Rust using Windows APIs, with an emphasis on fast startup, low resource usage,
portability, and a simple offline experience.

## Goals

- Preview common image and text formats directly from File Explorer
- Stay responsive while handling large or malformed files
- Run without a cloud service, account, or background server
- Ship as both a portable application and a simple per-user installer

## Requirements

- Windows 10 or Windows 11 x64
- Rust 1.97.1 with the MSVC target
- Visual Studio Build Tools with the Desktop development with C++ workload

## Build and check

```powershell
cargo run
cargo run -- --help
cargo run -- --input-diagnostics
cargo run -- --worker-diagnostics
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
cargo build --release
```
