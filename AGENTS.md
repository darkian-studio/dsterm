# AGENTS.md

## Build / tooling rules

- **On this machine (Termux/Android) `cargo build` / `cargo check` / `cargo test` /
  `cargo clippy` hang indefinitely.** Only `cargo fmt` is safe.
  - Allowed: `cargo fmt`
  - Forbidden (hangs): `cargo build`, `cargo check`, `cargo test`,
    `cargo run`, `cargo clippy`, `cargo install`, etc.
- On a normal desktop or CI runner these commands work fine.
  Use CI to catch compilation errors — it runs `cargo clippy --all-targets
  --all-features -- -D warnings` and `cargo test` on every push.
- Verify changes by reading the code and reasoning about correctness.
  Push to CI to confirm it compiles.
