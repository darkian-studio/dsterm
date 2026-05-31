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
| `-h, --help` | — | Print help information |
| `-V, --version` | — | Print version information |

## Commands

### `dsterm` (default — server mode)

Starts the main HTTP + WebSocket server. All API endpoints become available.

### `dsterm update`

Checks for a new release on GitHub. If one exists, downloads and replaces the current binary.

- Checks at most once per 24 hours (cached in `~/.cache/dsterm/.dsterm_update_cache`).
- Supports Android targets: `armv7`, `aarch64`, `x86_64`.

```bash
dsterm update
```

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

# Start with CORS disabled
dsterm --allow-any-origin

# Full combo
dsterm -p 8080 -i -c /usr/bin/bash --allow-any-origin

# Check for updates
dsterm update

# Start standalone LSP proxy
dsterm lsp rust-analyzer

# LSP proxy with session name on port 9090
dsterm lsp -s my-session -p 9090 rust-analyzer --some-lsp-flag
```
