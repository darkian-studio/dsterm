# AGENTS.md

## Build / tooling rules

- **Never run any `cargo` commands except `cargo fmt`.**
  - Allowed: `cargo fmt`
  - Forbidden: `cargo build`, `cargo check`, `cargo test`, `cargo run`,
    `cargo clippy`, `cargo install`, etc. Do not start these, and if one is
    somehow running, terminate it immediately.
- Verify changes by reading the code and reasoning about correctness, not by
  compiling or executing the crate.

> **Note:** The restriction above applies only on Windows. The crate does
> cross-compile and run there — `release.yml` builds `x86_64-pc-windows-msvc`
> on `windows-latest`, and the Windows server (ConPTY, port discovery, key
> files) is a supported target since 1.5.0. The restriction is about this
> machine's build cost, not a platform limitation: a `cargo build` here
> compiles the vendored llama.cpp tree for several minutes, and the
> `llama` feature additionally needs CMake and a C++17 toolchain.
>
> Two consequences of that, worth knowing before trusting a local run:
> `tests/transfer.rs` gates its symlink assertions behind `#[cfg(unix)]`, so
> they silently vanish on Windows, and `terminal/shell_integration.rs` plus
> `terminal/pty_fallback.rs` are `#[cfg]`-gated out entirely — the OSC 633
> and TIOCGPTPEER paths have no Windows coverage at all.


