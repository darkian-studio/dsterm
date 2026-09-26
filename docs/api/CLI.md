# CLI Reference & Server Info

## Usage

```text
dsterm [OPTIONS] [COMMAND]
```

## Global Options

| Flag | Default | Description |
| ------ | --------- | ------------- |
| `-p, --port <PORT>` | `8767` | Port to start the server (range: 1–65535) |
| `-i, --ip` | — | Bind to the first non-loopback IPv4 address instead of `127.0.0.1` |
| `-c, --command <COMMAND>` | `login` | Custom program/shell for interactive PTY sessions (e.g. `/usr/bin/bash`) |
| `--allow-any-origin` | — | Allow all CORS origins (dangerous — disables origin checks). Default restricts to `https://localhost` |
| `--remote` | — | Enable the filesystem API (`/fs/*`) with the current directory as the workspace root, no config file needed (sets `filesystem.enabled = true`) |
| `-h, --help` | — | Print help information |
| `-V, --version` | — | Print version information |

## Commands

### `dsterm` (default — server mode)

Starts the main HTTP + WebSocket server. All API endpoints become available.

#### Remote filesystem quickstart (`dsterm --remote`)

To open this machine's files from Darkian Studio with **zero configuration**:

```bash
dsterm --remote
```

This enables `/fs/*` using the current directory as the workspace root — no TOML,
no `--config`. In Darkian Studio choose **Open remote folder → Connect to local**
**dsterm** (or add a `dsterm` connection to `127.0.0.1:8767`).

For another machine on your LAN, bind a reachable address:

```bash
dsterm --remote -i
```

Then connect to that machine's IP on port `8767`. LAN mode is **unauthenticated**
**and cleartext** — use it only on networks you trust; for untrusted networks use
`dsterm host` (encrypted relay). To make it persistent, run `dsterm startup` —
its boot entry runs `dsterm host --remote` (relay host plus the `/fs/*` API) on
Android (Termux:Boot), Linux (systemd), macOS (launchd), and Windows (Startup
folder).

### `dsterm update`

Checks for a new release on GitHub. If one exists, downloads and replaces the current binary.

- Checks at most once per 24 hours (cached in `~/.cache/dsterm/.dsterm_update_cache`).
- Supports Android targets: `armv7`, `aarch64`, `x86_64`.

```bash
dsterm update
```

### `dsterm update status`

Reports the staged update candidate, if any — without touching anything:

```bash
dsterm update status
# ↓ Staged update: 1.9.2 (/usr/local/bin/dsterm.new)
# — or —
# ✓ No staged update. (running 1.9.1)
```

A candidate counts as staged only when **both** `<binary>.new` and its
sidecar (`<binary>.new.meta.json`) exist **and** the binary re-verifies
(size + sha256). A `.new` left behind by a crashed stage reads as "none",
never as ready. This is the durability behind `--self-update`: disk state
is the source of truth, so a missed push notification loses nothing.

### `dsterm --self-update` (server mode)

Opt-in supervised updating: launch with the flag and the background
update check, on finding a newer version, downloads it, verifies it with
the exact checks `dsterm update` applies (size, sha256 when published,
magic bytes), and stages it as `<binary>.new` — **without activating or
restarting**. Activation (rename over the live binary, restart) is a
separate, supervisor-driven step and deliberately out of scope here.

```bash
dsterm --self-update -p 8767
```

- Without the flag, launch behavior is byte-for-byte what it was
  (print-only update notice).
- The flag is server-mode only; combining it with any subcommand exits
  with code 2.
- An interrupted/failed stage never leaves a broken `.new` behind
  (temp file + atomic rename; partial state removed best-effort).

#### Supervisor contract: `update_ready` push

When staging finishes, `{"type": "update_ready", "version": "1.9.2"}` is
pushed as a JSON **text** frame to every currently open terminal
WebSocket — the same carrier as the existing `command_exit`/`exit`
control messages (binary frames stay pure PTY output, so clients must
keep distinguishing text control frames from binary output, as they
already do).

Best-effort by design: if no terminal is connected, the push goes
nowhere. Supervisors must treat `dsterm update status` (above) as the
authoritative query and the push as a wake-up hint. No supervisor
identity is required — any launcher that starts dsterm with
`--self-update` gets this behavior.

### `dsterm lsp <server> [args...]`

Starts a **standalone LSP WebSocket proxy**. See [BRIDGES.md](./BRIDGES.md#standalone-lsp-mode-dsterm-lsp) for full details.

| Flag | Description |
| ------ | ------------- |
| `-s, --session <ID>` | Session identifier for port discovery file |
| `<server>` | LSP server binary (e.g. `rust-analyzer`) |
| `[args...]` | Additional arguments forwarded to the server |

## Health Endpoints

These are available on the main server (port 8767 by default).

### GET /

```text
GET /
```

Returns the server identity string:

```text
Rust based DSTerm server
```

### GET /status

```text
GET /status
```

Simple liveness check. Returns:

```text
OK
```

## Examples

```bash
# Start on default port (localhost:8767)
dsterm

# Start on a custom port with a custom shell
dsterm -p 9090 -c /usr/bin/zsh

# Start on LAN IP
dsterm -i

# Zero-config remote filesystem for Darkian Studio (serves the current dir)
dsterm --remote

# Start with CORS disabled
dsterm --allow-any-origin

# Full combo
dsterm -p 8080 -i -c /usr/bin/bash --allow-any-origin

# Check for updates
dsterm update

# Staged-candidate status (for --self-update supervisors)
dsterm update status

# Supervised server: stage updates as dsterm.new, notify on ready
dsterm --self-update -p 8767

# Start standalone LSP proxy
dsterm lsp rust-analyzer

# LSP proxy with session name on port 9090
dsterm lsp -s my-session -p 9090 rust-analyzer --some-lsp-flag
```
