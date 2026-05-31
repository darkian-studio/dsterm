# Bridge Protocol APIs

DSTerm provides three protocol bridge modules — **LSP**, **DAP**, and **MCP** — that proxy between WebSocket clients and spawned server processes using the standard `Content-Length` framed protocol.

All three bridges share an identical pattern:

| Method | Endpoint | Description |
| -------- | ---------- | ------------- |
| `POST` | `/{protocol}/start` | Start a server process |
| `POST` | `/{protocol}/kill` | Kill one or all server sessions |
| `GET` | `/{protocol}/{id}` | WebSocket bridge to a running server |

---

## Framed Protocol

All three bridges use **Content-Length framing** (the standard LSP transport protocol).

### Encoding

Messages written to the server's stdin are prefixed with:

```text
Content-Length: <N>\r\n\r\n<JSON payload of exactly N bytes>
```

### Decoding

The server's stdout is read and parsed using the same format. Multiple messages may be concatenated in a single byte stream.

### Library reference

Use `crate::proto_frame::encode_frame(payload)` to produce framed bytes and `crate::proto_frame::FrameDecoder` to parse incoming byte streams.

---

## LSP

The LSP bridge has two modes: **server-mode bridge** (routes under `/lsp/*` on the main DSTerm server) and **standalone mode** (a separate HTTP server started via `dsterm lsp`).

### Server-Mode Bridge (`/lsp/*`)

Routes on the main DSTerm server (port 8767 by default).

#### Start LSP Server

```text
POST /lsp/start
```

**Request Body:**

```json
{
  "id": "my-lsp",
  "command": "rust-analyzer",
  "args": [],
  "cwd": "/project"
}
```

| Field | Type | Required | Description |
| ------- | ------ | ---------- | ------------- |
| `id` | string | Yes | Unique session identifier |
| `command` | string | Yes | LSP server binary to spawn |
| `args` | array of strings | Yes | Arguments to pass to the server |
| `cwd` | string | No | Working directory for the server |

**Response:**

```json
{
  "id": "my-lsp",
  "ws_path": "/lsp/my-lsp"
}
```

| Field | Type | Description |
| ------- | ------ | ------------- |
| `id` | string | Echoed session identifier |
| `ws_path` | string | WebSocket endpoint path to connect to |

**Errors:**

| Status | Condition |
| -------- | ----------- |
| 409 | Session ID already exists |
| 500 | Spawn failed |

#### Kill LSP Session(s)

```text
POST /lsp/kill
```

**Request Body (optional — empty body kills all):**

```json
{
  "id": "my-lsp"
}
```

| Field | Type | Required | Description |
| ------- | ------ | ---------- | ------------- |
| `id` | string | No | Session ID to kill. Omit to kill all sessions. |

**Response:**

```json
{
  "killed": ["my-lsp"]
}
```

| Field | Type | Description |
| ------- | ------ | ------------- |
| `killed` | array of strings | IDs of sessions that were killed |

#### LSP WebSocket Bridge

```text
GET /lsp/{id}
```

Upgrades to a WebSocket that proxies bidirectional framed protocol messages between the client and the LSP server process.

- **Client → Server**: text or binary messages are framed (`Content-Length`) and written to the server's stdin.
- **Server → Client**: server's stdout is parsed for framed messages and forwarded as WebSocket text frames.

Stderr from the LSP server is logged via `tracing::warn!` with target `lsp_stderr`.

When the WebSocket closes, the LSP process is killed and the session is removed.

**Errors:**

| Status | Condition |
| -------- | ----------- |
| 404 | Session ID not found |
| 409 | Stdout already claimed (only one WS client per session) |

---

### Standalone LSP Mode (`dsterm lsp`)

Run the DSTerm binary as a dedicated LSP proxy:

```bash
dsterm lsp [-s <session>] <server> [args...]
```

This starts a **separate HTTP server** (port auto-selected by default, or via `-p`) that:

#### WebSocket Bridge

```text
GET /
```

Upgrades to WebSocket, spawns the LSP server process per WebSocket connection, and proxies framed LSP messages bidirectionally.

The same framed protocol applies. Stderr is logged via `tracing::warn!` with target `lsp_stderr`.

#### Status

```text
GET /status
```

Returns information about all LSP processes managed by this proxy:

```json
{
  "program": "rust-analyzer",
  "processes": [
    {
      "pid": 12345,
      "uptime_secs": 3600,
      "memory_bytes": 52428800
    }
  ]
}
```

| Field | Type | Description |
| ------- | ------ | ------------- |
| `program` | string | LSP server binary name |
| `processes` | array | List of running process stats |
| `processes[].pid` | number | Process ID |
| `processes[].uptime_secs` | number | Seconds since process started |
| `processes[].memory_bytes` | number | Physical memory usage in bytes |

#### Port Discovery

In standalone mode, the actual listening port is written to:

```text
~/.dsterm/lsp_ports/<server_name>_<pid>
```

This file is automatically cleaned up when the server exits.

#### CLI Flags

| Flag | Description |
| ------ | ------------- |
| `-s, --session` | Session identifier for port discovery (allows multiple instances of same server) |
| `-p, --port` | Specify port explicitly (default: auto-select) |
| `-i, --ip` | Bind to LAN IP instead of localhost |
| `--allow-any-origin` | Disable CORS origin restriction |

---

## DAP

Routes under `/dap/*` on the main DSTerm server.

### Start DAP Server

```text
POST /dap/start
```

**Request Body:**

```json
{
  "id": "my-debugger",
  "command": "lldb-vscode",
  "args": [],
  "cwd": "/project"
}
```

| Field | Type | Required | Description |
| ------- | ------ | ---------- | ------------- |
| `id` | string | Yes | Unique session identifier |
| `command` | string | Yes | DAP server binary to spawn |
| `args` | array of strings | Yes | Arguments to pass to the server |
| `cwd` | string | No | Working directory for the server |

**Response:**

```json
{
  "id": "my-debugger",
  "ws_path": "/dap/my-debugger"
}
```

**Errors:**

| Status | Condition |
| -------- | ----------- |
| 409 | Session ID already exists |
| 500 | Spawn failed |

### Kill DAP Session(s)

```text
POST /dap/kill
```

**Request Body (optional — empty body kills all):**

```json
{
  "id": "my-debugger"
}
```

**Response:**

```json
{
  "killed": ["my-debugger"]
}
```

### DAP WebSocket Bridge

```text
GET /dap/{id}
```

Upgrades to a WebSocket that proxies bidirectional framed protocol messages between the client and the DAP server process.

Same protocol and behavior as the LSP bridge. Stderr is logged with target `dap_stderr`.

**Errors:**

| Status | Condition |
| -------- | ----------- |
| 404 | Session ID not found |
| 409 | Stdout already claimed |

---

## MCP

Routes under `/mcp/*` on the main DSTerm server.

### Start MCP Server

```text
POST /mcp/start
```

**Request Body:**

```json
{
  "id": "my-mcp",
  "command": "mcp-server",
  "args": [],
  "cwd": "/project"
}
```

| Field | Type | Required | Description |
| ------- | ------ | ---------- | ------------- |
| `id` | string | Yes | Unique session identifier |
| `command` | string | Yes | MCP server binary to spawn |
| `args` | array of strings | Yes | Arguments to pass to the server |
| `cwd` | string | No | Working directory for the server |

**Response:**

```json
{
  "id": "my-mcp",
  "ws_path": "/mcp/my-mcp"
}
```

**Errors:**

| Status | Condition |
| -------- | ----------- |
| 409 | Session ID already exists |
| 500 | Spawn failed |

### Kill MCP Session(s)

```text
POST /mcp/kill
```

**Request Body (optional — empty body kills all):**

```json
{
  "id": "my-mcp"
}
```

**Response:**

```json
{
  "killed": ["my-mcp"]
}
```

### MCP WebSocket Bridge

```text
GET /mcp/{id}
```

Upgrades to a WebSocket that proxies bidirectional framed protocol messages between the client and the MCP server process.

Same protocol and behavior as the LSP bridge. Stderr is logged with target `mcp_stderr`.

**Errors:**

| Status | Condition |
| -------- | ----------- |
| 404 | Session ID not found |
| 409 | Stdout already claimed |

---

## Examples

### JavaScript (Start + Connect Bridge)

```javascript
// Start an LSP server
const res = await fetch('http://localhost:8767/lsp/start', {
  method: 'POST',
  headers: { 'Content-Type': 'application/json' },
  body: JSON.stringify({
    id: 'analyzer',
    command: 'rust-analyzer',
    args: []
  })
});
const { ws_path } = await res.json();

// Connect WebSocket bridge
const ws = new WebSocket(`ws://localhost:8767${ws_path}`);

// Send a framed LSP message
function sendLspMessage(payload) {
  const frame = `Content-Length: ${payload.length}\r\n\r\n${payload}`;
  ws.send(frame);
}

ws.onmessage = (event) => {
  // Parse framed responses
  console.log('Received:', event.data);
};

// Initialize LSP session
sendLspMessage(JSON.stringify({
  jsonrpc: '2.0',
  id: 1,
  method: 'initialize',
  params: { ... }
}));
```

### Python (Start + Connect Bridge)

```python
import requests
import json
import websocket

# Start DAP server
res = requests.post('http://localhost:8767/dap/start', json={
    'id': 'debug1',
    'command': 'lldb-vscode',
    'args': []
})
ws_path = res.json()['ws_path']

# Connect bridge
ws = websocket.WebSocket()
ws.connect(f'ws://localhost:8767{ws_path}')

# Send framed message
payload = json.dumps({
    'seq': 1,
    'type': 'request',
    'command': 'initialize',
    'arguments': {...}
})
frame = f'Content-Length: {len(payload)}\r\n\r\n{payload}'
ws.send(frame)

# Receive framed response
response = ws.recv()
print(response)
```

### cURL (Manage Sessions)

```bash
# Start
curl -X POST http://localhost:8767/mcp/start \
  -H "Content-Type: application/json" \
  -d '{"id":"mcp1","command":"mcp-server","args":[]}'

# Kill specific
curl -X POST http://localhost:8767/mcp/kill \
  -H "Content-Type: application/json" \
  -d '{"id":"mcp1"}'

# Kill all
curl -X POST http://localhost:8767/mcp/kill
```

### Standalone LSP (CLI)

```bash
# Start proxy for rust-analyzer on auto-selected port
dsterm lsp rust-analyzer

# Start with explicit session and port
dsterm lsp -s v1 -p 9090 rust-analyzer

# Check status
curl http://localhost:9090/status
```
