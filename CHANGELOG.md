# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [1.5.1] - 2026-07-11

### Fixed
- Windows releases no longer link the WinPcap-only `Packet.lib`; LAN IPv4
  discovery uses the native IP Helper adapter API.

## [1.5.0] - 2026-07-11

### Added
- Native Windows server support: ConPTY terminals, Windows port discovery and
  process termination, relay host identity, and secure key-file handling.

### Changed
- Version bumped from `1.4.3` to `1.5.0`.

## [1.4.3] - 2026-07-10

### Added
- macOS and Windows release binaries built in CI (`dsterm-macos-arm64`,
  `dsterm-macos-x86_64`, `dsterm-windows-x86_64.exe`).
- Platform-agnostic installers: `install.sh` now covers Termux/Linux/macOS with a
  `cargo` fallback, and a new `install.ps1` installs on Windows.

### Changed
- `dsterm update` now resolves macOS/Windows assets and safely replaces the
  running binary on Windows (move-aside then swap).
- Version bumped from `1.4.2` to `1.4.3`.

## [1.4.2] - 2026-07-10

### Added
- `dsterm startup` now supports macOS (launchd LaunchAgent) and Windows (per-user
  Startup-folder script) in addition to Termux:Boot and systemd.

### Changed
- Autostart entries now run `dsterm host --remote` (relay host plus the
  zero-config `/fs/*` API) on all platforms.
- Version bumped from `1.4.1` to `1.4.2`.

## [1.4.1] - 2026-07-10

### Changed
- `--remote` startup banner now prints a structured summary (Remote file system
  enabled / IP / Port / Folder) before the normal server logs.
- Version bumped from `1.4.0` to `1.4.1`.

## [1.4.0] - 2026-07-10

### Added
- Global `--remote` flag that enables the filesystem API (`/fs/*`) using the
  current directory as the workspace root with no config file, plus a startup
  banner. Documented in CLI.md and FILESYSTEM.md.

### Changed
- Version bumped from `1.3.0` to `1.4.0`.

## [1.3.0] - 2026-07-10

### Added
- Relay routing for ACP agents (`agents:start`, `agents:input`, `agents:kill`) with
  `agent:output` / `agent:exit` streaming over the encrypted relay.
- Relay routing for localhost WebSocket tunneling (`ws:open`, `ws:data`, `ws:close`).
- Relay terminal re-attach (`terminal:attach`) to an existing PTY, replaying scrollback.
- Relay system-monitor push (`sysmon:subscribe` / `sysmon:unsubscribe`) emitting
  periodic `sysmon:update` snapshots.

### Changed
- Version bumped from `1.2.0` to `1.3.0`.

## [1.2.0] - 2026-07-09

### Added
- Filesystem directory listing endpoint and relay routing for `fs:list`.
- Relay routing for `http:request` over `/proxy/http`.
- Proxy, agent bridge, relay, and startup API docs.

### Changed
- Version bumped from `1.1.0` to `1.2.0`.

## [1.1.0] - 2026-07-09

### Added
- Phase 3 relay host surface: proxy, agent bridge, relay transport, and startup installer.
- Filesystem, system monitor, pairing, relay, proxy, agents, and host-mode API docs.

### Changed
- Version bumped from `1.0.0` to `1.1.0`.

## [1.0.0] - 2026-06-XX

### Added
- **TOML configuration file** — `--config <path>` flag loads a TOML file at startup.
  All previously hardcoded values (`max_scrollback_bytes`, `output_coalesce_ms`,
  `read_buffer_bytes`, `inactivity_timeout_secs`, `kill_timeout_secs`) are now
  configurable. Defaults are identical to previous behaviour.
- **`/metrics` endpoint** — Prometheus text format exposing
  `dsterm_terminal_sessions_total` (counter) and `dsterm_terminal_sessions_active`
  (gauge). No additional dependency required.
- **Graceful shutdown** — SIGTERM (Linux/Android) and SIGINT (all platforms) drain
  in-flight requests before exiting. Previously the process was hard-killed.
- **Terminal session inactivity eviction** — sessions idle for longer than
  `inactivity_timeout_secs` (default 30 min) are automatically killed and cleaned up.
- **Linux static binary support** — release publishes `dsterm-linux-{arm64,armv7,x86_64}`
  musl static binaries that run on any Linux distribution without glibc constraints.
- **Linux install path** — `install.sh` detects non-Termux Linux environments and
  downloads the correct `dsterm-linux-*` binary.

### Changed
- Release binary naming: `dsterm-musl-android-*` renamed to `dsterm-linux-*`.
  Android/Termux users continue to receive `dsterm-android-*` binaries unchanged.
- `dsterm update` now detects Termux vs. regular Linux and downloads the appropriate
  binary for the running environment.
- Version bumped from `0.8.1` to `1.0.0`.

---

## [0.8.1] - 2026-06-08

### Added
- Working directory (`cwd`) support for `POST /terminals`.
- Bug fixes to PTY session lifecycle.

## [0.8.0] - 2026-06-08

### Added
- `GET /terminals` — list all active terminal sessions.
- Terminal feature refinements.

## [0.7.0] - 2026-05-31

### Added
- Terminal listing endpoint infrastructure.

## [0.6.0] - 2026-05-27

### Added
- Extension Host bridge (`POST /extension-host/start`, `POST /extension-host/kill`,
  `GET /extension-host/{id}`) — newline-delimited JSON protocol over WebSocket.

## [0.5.0] - 2026-05-25

### Added
- Shell integration (bash, zsh, fish) injected into interactive PTY sessions.
- OSC 633 exit code tracking — emits `{"type":"command_exit","exit_code":N}` frames.
- AST bridge (`POST /ast/scope`) — tree-sitter scope chain analysis for Python,
  JavaScript, and TypeScript with 256-entry LRU document cache.
- Silent execution streaming (`GET /silent-exec-stream` WebSocket) — chunked
  stdout/stderr with timeout support.

## [0.4.0] - 2026-05-15

### Added
- `docs/api/` — API reference documentation for all endpoints.

## [0.3.3] - 2026-05-12

### Added
- LSP bridge, DAP bridge, and MCP bridge — Content-Length framed protocol proxies
  (`/lsp/*`, `/dap/*`, `/mcp/*`).
- Standalone LSP proxy mode (`dsterm lsp <server>`).
- `proto_frame` — shared Content-Length frame encoder/decoder.

## [0.3.0] - 2026-05-12

### Added
- Initial PTY management over WebSocket (`POST /terminals`, `GET /terminals/{pid}`,
  `POST /terminals/{pid}/resize`, `POST /terminals/{pid}/terminate`).
- Execute command endpoint (`POST /execute-command`).
- Silent execution endpoint (`POST /silent-exec`).
- Automatic update checking on server start (`dsterm update`).
- Multi-platform CI and release workflow (Android arm64/armv7/x86_64).
