# CursorPeek fuzzing

These targets exercise the same platform-neutral parsing code linked into `CursorPeek.exe`.
Normal Windows builds use the stable toolchain pinned at the repository root. Coverage-guided
fuzzing uses the separate dated nightly in this directory because `cargo-fuzz` and libFuzzer run
on supported Unix-like hosts, not natively on Windows.

Install the pinned runner on Linux or WSL:

```sh
cargo install cargo-fuzz --version 0.13.2 --locked
```

From this directory, run a bounded target:

```sh
cargo fuzz run protocol -- -max_len=4194328 -timeout=5 -rss_limit_mb=768
cargo fuzz run payload -- -max_len=4194304 -timeout=5 -rss_limit_mb=768
cargo fuzz run content_sniff -- -max_len=65537 -timeout=5 -rss_limit_mb=768
cargo fuzz run layout -- -max_len=16 -timeout=5 -rss_limit_mb=768
```

The directories under `corpus` are versioned regression seeds. Generated crashes live under
`artifacts` and are ignored. Reproduce and minimize a failure before copying the smallest input
into the matching corpus directory:

```sh
cargo fuzz run protocol artifacts/protocol/<artifact>
cargo fuzz tmin protocol artifacts/protocol/<artifact>
```

`cargo test --locked --workspace` replays every retained seed on Windows without nightly or
`cargo-fuzz`.
