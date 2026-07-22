# Security Policy

## Overview

DSTerm exposes pseudo-terminals, filesystem access, command execution, and protocol bridges over HTTP/WebSocket. This document describes the security model, threat surface, and hardening measures built into the codebase.

---

## End-to-End Encryption (Relay Mode)

When running via `dsterm host`, all client traffic is **end-to-end encrypted** using XSalsa20-Poly1305 (NaCl secretbox). The relay server forwards ciphertext and never sees plaintext.

- **Key generation**: a 32-byte symmetric key is generated on first run and persisted to `~/.dsterm/dsterm-<machine-id>.e2ee` with mode `0600` (owner-read-only on Unix).
- **Nonces**: every message is encrypted with a fresh 24-byte random nonce. Nonces are never reused under the same key.
- **Wire format**: encrypted messages travel as JSON envelopes with `nonce` and `ciphertext` fields (both base64-encoded). The relay server routes these opaquely.
- **Plaintext exception**: only `ping`, `pong`, and `session:*` control messages are sent unencrypted. These contain no user data.

### Pairing

The `dsterm pair` command produces a QR code containing `hostId:keyBase64`. This is the only point where the E2E key is exposed. Scan it once with the client app; after that, all communication is encrypted.

**Risk**: anyone who scans the QR code or reads the key file can impersonate the host. Protect the key file and treat QR codes as secret.

---

## Client Approval System

When a new client connects via the relay, the host decides whether to allow it based on `security.unknown_clients`:

| Policy | Behavior |
|---|---|
| `always-allow` | All clients are approved immediately |
| `always-reject` | All unknown clients are rejected |
| `requires-approval` (default) | Unknown clients are marked `Pending` until the host operator runs `dsterm clients approve <id>` |

Client records are persisted in `~/.dsterm/clients.json` with `firstSeen`, `lastSeen`, platform, and app version metadata.

**Only approved clients** can send encrypted command messages. Unapproved client messages are silently dropped (`transport.rs:138-140`).

---

## Filesystem API

The `/fs/*` endpoints are **disabled by default**. Enable via:

- Config: `[filesystem] enabled = true`
- CLI flag: `--remote` (sets `enabled = true` and uses CWD as workspace root)

### Path Traversal Protection

All filesystem operations resolve the requested path through `safe_path()`, which:

1. Joins relative paths against the configured workspace root.
2. Applies lexical normalization (resolves `..` and `.` without touching the filesystem).
3. Verifies the result **starts with** the workspace root. Requests that escape the root are rejected with `"Path escapes workspace root"`.

### Read Limits

`filesystem.max_read_bytes` (default 2 MB) caps individual file reads. Files exceeding this limit return HTTP 413.

### Binary File Handling

Files containing null bytes are automatically detected and returned as base64 with `"encoding": "base64"`. This prevents binary content from being mangled by UTF-8 interpretation.

### Workspace Root Deletion

`POST /fs/delete` refuses to delete the workspace root itself, preventing catastrophic self-deletion.

---

## Command Execution

### Silent Exec (`POST /silent-exec`)

Runs a command via `sh -c` (Unix) or `cmd /C` (Windows) with piped stdout/stderr. Key properties:

- **Timeout**: configurable per-request (default 30s, max enforced by caller). On timeout, the child process is killed.
- **No PTY**: uses standard process I/O, so there is no terminal emulation overhead or escape-sequence injection surface.
- **Working directory validation**: rejects non-existent CWD paths before spawning.

### Streaming Exec (`GET /silent-exec-stream`)

WebSocket variant that streams stdout/stderr chunks in real time. Same validation as silent exec.

### Execute Command (`POST /execute-command`)

Runs a command inside a PTY with a 30-second timeout. ANSI escape sequences are stripped from the output before returning.

---

## PTY Security

### TIOCGPTPEER Fallback

When the standard `openpty()` call fails (e.g., SELinux blocks `open("/dev/pts/N")`), DSTerm falls back to the Linux `TIOCGPTPEER` ioctl. This path:

1. Opens `/dev/ptmx` with `O_RDWR | O_CLOEXEC`.
2. Calls `grantpt()` and `unlockpt()`.
3. Obtains the slave fd directly from the master via `ioctl(TIOCGPTPEER)`, completely bypassing `/dev/pts`.
4. In `pre_exec`, the child process resets signal dispositions to `SIG_DFL`, clears the signal mask, calls `setsid()` to create a new session, calls `ioctl(0, TIOCSCTTY, 0)` to set the controlling terminal, and closes all file descriptors above stderr via `close_range(2)` (Linux 5.9+) or a fallback loop.

This ensures proper session isolation even when the standard PTY path is restricted.

### File Descriptor Leak Prevention

The `FallbackMasterPty` sets `FD_CLOEXEC` on all cloned file descriptors to prevent them from leaking into spawned child processes.

### EOT on Writer Drop

The fallback writer sends the terminal's `VEOF` character (typically Ctrl-D) on drop, matching `portable-pty` behavior and ensuring clean session termination.

---

## Shell Integration

Per-session shell integration files (bash, zsh, fish) are written to temporary directories under `$TMPDIR/dsterm-integration-<uuid>/`. These files source OSC 633 escape code hooks for exit-status tracking, are cleaned up when the session ends, and use unique UUIDs per session to prevent cross-session interference.

For zsh, `ZDOTDIR` is overridden to point at the per-session directory, preventing the session from loading the user's `.zshrc`.

---

## Scrollback Isolation

Each terminal session's scrollback is stored in a separate file: `$TMPDIR/dsterm_scrollback_<pid>.bin`. Files are created with append-only access, deleted on session termination or eviction, and cleaned up on `Drop`. Scrollback is not shared between sessions.

---

## Inactivity Eviction

A background task runs every 60 seconds and evicts terminal sessions that have been idle for longer than `terminal.inactivity_timeout_secs` (default 30 minutes). Eviction kills the child process and removes the scrollback file.

---

## CORS Policy

By default, CORS allows only `https://localhost`. Use `--allow-any-origin` to disable this restriction (dangerous in production).

---

## Proxy Security

The HTTP and WebSocket proxy (`/proxy/*`) is **disabled by default**. When enabled, only **localhost targets** are allowed. The `is_localhost()` check validates against `localhost`, `::1`, `[::1]`, and `127.*.*.*`. Non-localhost requests are rejected with HTTP 400. This prevents the proxy from being used as an open relay to external services.

---

## Port Killing

`POST /ports/kill` is **disabled by default**. When enabled via `[ports] kill_enabled = true`, it can terminate processes by port number. On Linux it parses `/proc/net/tcp*` to map ports to socket inodes, then maps inodes to PIDs via `/proc/*/fd` symlinks, and sends `SIGKILL`. On Windows it uses `GetExtendedTcpTable` + `TerminateProcess`.

---

## Update Verification

The `dsterm update` command performs multiple verification steps before replacing the running binary:

1. **Size check**: downloaded bytes must match the asset's `size` field.
2. **SHA-256 checksum**: if a `.sha256` asset exists, the downloaded binary must match.
3. **Platform validation** (Windows): the PE header (`MZ`) is verified to prevent ELF binaries from being installed on Windows.

If any check fails, the update is aborted and the installed binary is **not** touched.

---

## Key File Permissions

On Unix, the E2E key file is created with mode `0600` (owner-read-write only). On Windows, the default NTFS DACL on `%USERPROFILE%` provides similar user-restriction.

---

## Threat Model Summary

| Attack Vector | Mitigation |
|---|---|
| Relay server compromise | E2E encryption; server sees only ciphertext |
| Unauthorized client | Client approval system with `requires-approval` default |
| Path traversal on filesystem | `safe_path()` lexical normalization + root prefix check |
| Command injection via exec | Commands run via `sh -c`; CWD validated before spawn |
| PTY file descriptor leak | `FD_CLOEXEC` on all cloned fds; `close_range` in `pre_exec` |
| Open proxy to external hosts | Proxy restricted to localhost targets only |
| Binary replacement via update | Size + SHA-256 + PE header verification |
| Key theft | 0600 permissions; key only exposed during pairing QR scan |
| Scrollback data leak | Per-session temp files, deleted on session end |
| Idle session abuse | Automatic eviction after configurable inactivity timeout |

---

## Reporting Vulnerabilities

If you discover a security vulnerability, please report it responsibly by opening a private issue or contacting the maintainers at **contact@darkian.io**. Do not disclose vulnerabilities publicly until a fix is available.
