# DSTerm

`dsterm` is a Rust-based backend server that exposes a pseudo-terminal (PTY),
protocol bridges, and command execution over HTTP and WebSocket.
It runs anywhere you have a terminal — **Termux on Android**, any **Linux** distribution,
or any environment with a shell.

## Features

- **Interactive PTY** — full terminal sessions streamed over WebSocket
- **Shell integration** — OSC 633 exit-code tracking for bash, zsh, and fish
- **Silent execution** — non-interactive command runner with stdout/stderr capture
- **Streaming execution** — chunked output streamed over WebSocket
- **LSP bridge** — WebSocket proxy to any Language Server Protocol server
- **DAP bridge** — WebSocket proxy to any Debug Adapter Protocol server
- **MCP bridge** — WebSocket proxy to any Model Context Protocol server
- **Extension Host bridge** — Node.js process bridge with newline-delimited JSON
- **AST bridge** — tree-sitter scope analysis for Python, JavaScript, TypeScript
- **Prometheus metrics** — `/metrics` endpoint with session counters
- **TOML configuration** — optional config file for all tunable parameters
- **Graceful shutdown** — drains sessions on SIGTERM / SIGINT
- **Automatic updates** — `dsterm update` downloads the latest binary

## Installation

**Termux (Android), Linux, macOS** (bash):

```bash
curl -L https://raw.githubusercontent.com/darkian-studio/dsterm/main/install.sh | bash
```

**Windows** (PowerShell):

```powershell
irm https://raw.githubusercontent.com/darkian-studio/dsterm/main/install.ps1 | iex
```

Both installers detect your platform automatically and download the matching
prebuilt binary from the latest release:

- **Termux (Android)** — native Android binary (arm64, armv7, x86_64)
- **Linux** — static musl binary (x86_64, aarch64, armv7) that runs on any
  distribution without glibc version constraints
- **macOS** — native binary (Apple Silicon arm64, Intel x86_64)
- **Windows** — native binary (x86_64)

When no prebuilt binary matches your platform/architecture, the installers fall
back to building from source with `cargo` (requires a [Rust toolchain](https://rustup.rs)):

```sh
cargo install --git https://github.com/darkian-studio/dsterm dsterm
```

## Update

`dsterm` checks for updates on every start and notifies you when one is available.
To update immediately:

```sh
dsterm update
```

## Usage

```text
dsterm [OPTIONS] [COMMAND]

Commands:
  update  Check for and install the latest release
  lsp     Start a standalone WebSocket LSP bridge
  help    Print help

Options:
  -p, --port <PORT>            Server port [default: 8767]
  -i, --ip                     Bind to LAN IP instead of 127.0.0.1
  -c, --command <CMD>          Custom shell / program for PTY sessions
      --config <PATH>          TOML configuration file
      --allow-any-origin       Disable CORS origin restriction (dangerous)
  -h, --help                   Print help
  -V, --version                Print version
```

### Examples

```bash
# Start on default port (localhost:8767)
dsterm

# Start with a custom shell
dsterm -c /usr/bin/zsh

# Start on LAN IP with a config file
dsterm -i --config /etc/dsterm.toml

# Standalone LSP proxy for rust-analyzer
dsterm lsp rust-analyzer

# Check for updates
dsterm update
```

## Configuration

Create a TOML file and pass it with `--config`:

```toml
[terminal]
max_scrollback_bytes = 262144   # 256 KB per session
output_coalesce_ms = 8          # WebSocket flush interval
read_buffer_bytes = 8192        # PTY read buffer size
inactivity_timeout_secs = 1800  # Evict idle sessions after 30 min

[bridges]
kill_timeout_secs = 2           # Grace period when killing bridge processes
```

All fields are optional — omitted fields use the defaults shown above.

## API Reference

Full documentation lives in `[docs/api/](docs/api/)`:

| Document | Contents |
| --- | --- |
| `[CLI.md](docs/api/CLI.md)` | CLI flags and health endpoints |
| `[TERMINAL.md](docs/api/TERMINAL.md)` | Interactive PTY API |
| `[EXECUTE_COMMAND.md](docs/api/EXECUTE_COMMAND.md)` | `POST /execute-command` |
| `[SILENT_EXECUTION.md](docs/api/SILENT_EXECUTION.md)` | Silent exec + streaming |
| `[BRIDGES.md](docs/api/BRIDGES.md)` | LSP / DAP / MCP / Extension Host bridges |
| `[AST.md](docs/api/AST.md)` | AST scope endpoint |

## Observability

`GET /metrics` returns Prometheus-compatible metrics:

```text
# HELP dsterm_terminal_sessions_total Terminal sessions created since startup
# TYPE dsterm_terminal_sessions_total counter
dsterm_terminal_sessions_total 42
# HELP dsterm_terminal_sessions_active Currently active terminal sessions
# TYPE dsterm_terminal_sessions_active gauge
dsterm_terminal_sessions_active 3
```

## Building from Source

```bash
git clone https://github.com/darkian-studio/dsterm.git
cd dsterm
cargo build --release
# Binary: target/release/dsterm
```

Rust stable toolchain required.

### Local inference (`llama` feature)

Local LLM inference via a vendored llama.cpp is an opt-in Cargo feature:

```bash
cargo build --release --features llama
```

System requirements for the `llama` feature:

- **CMake ≥ 3.14** and a **C++17 compiler**
- Termux: `pkg install cmake binutils` (g++/clang included)
- Linux: `g++` or `clang`
- macOS: Xcode Command Line Tools
- Windows: MSVC Build Tools

Notes:

- Release binaries ship with `llama` baked in. The `install.sh`/`install.ps1`
  source-build fallback installs **without** the feature by default — run
  `cargo install --git https://github.com/darkian-studio/dsterm --features llama` explicitly
  if you need local inference there.
- Without the feature, every inference endpoint returns an explicit
  `INTERNAL_SERVER_ERROR` ("Inference backend not compiled...") instead of a
  fake empty success.

> [!NOTE]
> If you encounter any issues, please open an issue on GitHub.
