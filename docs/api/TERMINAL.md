# Interactive Terminal API

Create, connect to, resize, and terminate PTY-based interactive terminal sessions.

## Create Terminal

```
POST /terminals
```

Spawns a shell (`login` by default, or a custom program set via `-c` flag) inside a new pseudo-terminal (PTY) and returns its PID.

### Request Body

```json
{
  "cols": 80,
  "rows": 24
}
```

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `cols` | number or string | Yes | Terminal width in columns |
| `rows` | number or string | Yes | Terminal height in rows |

### Response

```
200 OK
12345
```

The response body is a plain text string containing the PID of the spawned child process.

### Errors

```json
{
  "error": "Failed to open PTY: ..."
}
```

| Status | Condition |
|--------|-----------|
| 500 | PTY open failed (both `portable-pty` and TIOCGPTPEER fallback exhausted) |
| 500 | Command spawn failed (e.g. program not found) |
| 500 | PTY reader/writer cloning failed |

---

## Terminal WebSocket

```
GET /terminals/{pid}
```

Upgrades to a WebSocket connection for an interactive terminal session. The PID must match a value returned from `POST /terminals`.

### Protocol

Once upgraded, the connection uses **binary frames** for terminal I/O and **text frames** for control messages.

#### Client → Server (Binary)

Any binary data sent by the client is written directly to the PTY master. This is typically user keystrokes.

#### Server → Client (Binary)

The server streams PTY output as raw binary data. Output is **coalesced** to reduce frame overhead:
- A flush is triggered every **8 milliseconds** if data is buffered.
- A flush is also triggered immediately if the coalesce buffer reaches **8 KB**.

#### Reconnect / Scrollback Replay

When a new WebSocket connects to an existing terminal session, the server:

1. **Sends the scrollback tail** as a single binary message (up to 256 KB of recent PTY output). The client should clear its display before connecting.
2. **Sends** a JSON replay-complete signal (at application level):
   ```json
   {"type": "replay_complete"}
   ```
   *Note: the source currently does not emit this JSON — scrollback is sent as raw binary with no delimiter. Check server version for exact behavior.*

After replay, live PTY output streaming begins.

#### Command Exit (Shell Integration)

For sessions started with default settings, dsterm injects a shell‑integration
script (bash/zsh/fish) that emits `OSC 633 ; D ; <exit_code> ST` at every
prompt. dsterm strips these escapes from the binary stream and emits one
text frame per command:

```json
{"type":"command_exit","exit_code":0}
```

The IDE pairs this with whatever command it most recently sent. Integration
is disabled when dsterm is started with `-c <custom-command>`.

#### Process Exit

When the spawned process exits, the server sends:

```json
{
  "type": "exit",
  "data": {
    "exit_code": 0,
    "signal": null,
    "message": "Process exited successfully"
  }
}
```

| Field | Type | Description |
|-------|------|-------------|
| `exit_code` | number or null | Process exit code |
| `signal` | string or null | Signal that terminated the process (if any) |
| `message` | string | Human-readable exit description |

After sending the exit message, the session is removed and the WebSocket is closed.

### Errors

| Status | Condition |
|--------|-----------|
| 404 | PID not found (no active session) |

---

## Resize Terminal

```
POST /terminals/{pid}/resize
```

Resizes the PTY dimensions for an active terminal session.

### Request Body

```json
{
  "cols": 132,
  "rows": 43
}
```

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `cols` | number or string | Yes | New width in columns |
| `rows` | number or string | Yes | New height in rows |

### Response

```json
{
  "success": true
}
```

### Errors

```json
{
  "error": "Session not found"
}
```

```json
{
  "error": "Failed to resize: ..."
}
```

| Status | Condition |
|--------|-----------|
| 200 (with error body) | Session not found or resize ioctl failed |
| 500 | Internal error |

---

## Terminate Terminal

```
POST /terminals/{pid}/terminate
```

Kills the child process of an active terminal session and cleans up the scrollback file.

### Response

```json
{
  "success": true
}
```

### Errors

```json
{
  "error": "Session not found"
}
```

```json
{
  "error": "Failed to terminate terminal {pid}: ..."
}
```

| Status | Condition |
|--------|-----------|
| 200 (with error body) | Session not found or kill failed |

---

## Examples

### Create + Connect (JavaScript)

```javascript
// 1. Create terminal
const res = await fetch('http://localhost:8767/terminals', {
  method: 'POST',
  headers: { 'Content-Type': 'application/json' },
  body: JSON.stringify({ cols: 80, rows: 24 })
});
const pid = await res.text();

// 2. Connect WebSocket
const ws = new WebSocket(`ws://localhost:8767/terminals/${pid}`);
ws.binaryType = 'arraybuffer';

ws.onmessage = (event) => {
  if (event.data instanceof ArrayBuffer) {
    // PTY output — render to terminal
  } else {
    // JSON control message
    const msg = JSON.parse(event.data);
    if (msg.type === 'exit') {
      console.log('Process exited:', msg.data);
    }
  }
};

// 3. Send input
ws.send(new TextEncoder().encode('ls -la\r\n'));

// 4. Resize
await fetch(`http://localhost:8767/terminals/${pid}/resize`, {
  method: 'POST',
  headers: { 'Content-Type': 'application/json' },
  body: JSON.stringify({ cols: 132, rows: 43 })
});

// 5. Terminate
await fetch(`http://localhost:8767/terminals/${pid}/terminate`, {
  method: 'POST'
});
```

### cURL

```bash
# Create
PID=$(curl -s -X POST http://localhost:8767/terminals \
  -H "Content-Type: application/json" \
  -d '{"cols":80,"rows":24}')

# Resize
curl -s -X POST "http://localhost:8767/terminals/$PID/resize" \
  -H "Content-Type: application/json" \
  -d '{"cols":132,"rows":43}'

# Terminate
curl -s -X POST "http://localhost:8767/terminals/$PID/terminate"
```

### Python

```python
import requests
import websocket

# Create
res = requests.post('http://localhost:8767/terminals',
    json={'cols': 80, 'rows': 24})
pid = res.text

# Connect
ws = websocket.WebSocket()
ws.connect(f'ws://localhost:8767/terminals/{pid}')

# Send command
ws.send_binary(b'ls -la\r\n')

# Receive output
output = ws.recv()
print(output)

# Close
ws.close()
```
