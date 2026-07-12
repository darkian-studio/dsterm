# AGENTS.md

## Build / tooling rules

- **Never run any `cargo` commands except `cargo fmt`.**
  - Allowed: `cargo fmt`
  - Forbidden: `cargo build`, `cargo check`, `cargo test`, `cargo run`,
    `cargo clippy`, `cargo install`, etc. Do not start these, and if one is
    somehow running, terminate it immediately.
- Verify changes by reading the code and reasoning about correctness, not by
  compiling or executing the crate.
