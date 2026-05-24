# Silent Execution API

Execute commands without an interactive PTY. Two modes: **REST** (full result at once) and **WebSocket** (streaming output).

---

## REST: Silent Command Execution

```
POST /silent-exec
```

Runs `sh -c <command>` with piped stdout/stderr (no PTY) and returns the complete result.

### Request Body

```json
{
  "type": "silent_exec",
  "id": "unique-request-id",
  "command": "ls -la",
  "cwd": "/data/data/com.termux/files/home",
  "env": {
    "KEY": "value"
  },
  "timeout_ms": 30000
}
```

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `type` | string | No | Message type identifier |
| `id` | string | Yes | Unique request identifier (echoed in response) |
| `command` | string | Yes | Command to execute |
| `cwd` | string | No | Working directory (defaults to `$HOME`) |
| `env` | object | No | Additional environment variables |
| `timeout_ms` | number | No | Timeout in milliseconds (default: 30000) |

### Response

```json
{
  "type": "silent_exec_result",
  "id": "unique-request-id",
  "success": true,
  "exit_code": 0,
  "stdout": "total 12\ndrwxr-xr-x 2 user user 4096 Jan 1 12:00 .\n",
  "stderr": "",
  "timed_out": false
}
```

| Field | Type | Description |
|-------|------|-------------|
| `type` | string | Always `"silent_exec_result"` |
| `id` | string | Echoed request identifier |
| `success` | boolean | `true` if exit code is 0 |
| `exit_code` | number | Process exit code (`-1` on error or timeout) |
| `stdout` | string | Standard output |
| `stderr` | string | Standard error |
| `timed_out` | boolean | `true` if the command was killed due to timeout |

### Errors

```json
{
  "type": "silent_exec_result",
  "id": "request-id",
  "success": false,
  "exit_code": -1,
  "stdout": "",
  "stderr": "Empty command string",
  "timed_out": false
}
```

| Status | Condition |
|--------|-----------|
| 400 | Empty command or working directory does not exist |
| 500 | Spawn failure or internal error |

---

## WebSocket: Silent Command Execution (Streaming)

```
GET /silent-exec-stream
```

Standard WebSocket upgrade. Sends a single request message, receives streaming output chunks, and a final done message.

### Connection

Standard WebSocket upgrade at `ws://host:port/silent-exec-stream`.

### Send (first message after connect)

```json
{
  "type": "silent_exec",
  "id": "unique-request-id",
  "command": "ls -la",
  "cwd": "/data/data/com.termux/files/home",
  "timeout_ms": 60000
}
```

All fields match the REST request. The default timeout is **60000 ms** (60s) for streaming.

### Receive — stdout chunk

```json
{
  "type": "silent_exec_chunk",
  "id": "unique-request-id",
  "stream": "stdout",
  "data": "total 12\n"
}
```

### Receive — stderr chunk

```json
{
  "type": "silent_exec_chunk",
  "id": "unique-request-id",
  "stream": "stderr",
  "data": "error message\n"
}
```

### Receive — done

```json
{
  "type": "silent_exec_done",
  "id": "unique-request-id",
  "exit_code": 0,
  "timed_out": false
}
```

| Field | Type | Description |
|-------|------|-------------|
| `type` | string | `"silent_exec_chunk"` or `"silent_exec_done"` |
| `id` | string | Echoed request identifier |
| `stream` | string | `"stdout"` or `"stderr"` (only on chunks) |
| `data` | string | Line of output (only on chunks) |
| `exit_code` | number | Process exit code (only on done) |
| `timed_out` | boolean | Whether the command timed out (only on done) |

Output is sent **line by line** — each WebSocket text frame contains one line.

---

## Examples

### cURL (REST)

```bash
curl -X POST http://localhost:8767/silent-exec \
  -H "Content-Type: application/json" \
  -d '{"id": "1", "command": "echo hello"}'
```

### JavaScript (REST)

```javascript
const res = await fetch('http://localhost:8767/silent-exec', {
  method: 'POST',
  headers: { 'Content-Type': 'application/json' },
  body: JSON.stringify({ id: '1', command: 'echo hello' })
});
const result = await res.json();
console.log(result.stdout);
```

### JavaScript (WebSocket Streaming)

```javascript
const ws = new WebSocket('ws://localhost:8767/silent-exec-stream');

ws.onopen = () => {
  ws.send(JSON.stringify({
    type: 'silent_exec',
    id: '1',
    command: 'echo hello'
  }));
};

ws.onmessage = (event) => {
  const msg = JSON.parse(event.data);
  switch (msg.type) {
    case 'silent_exec_chunk':
      console.log(`[${msg.stream}] ${msg.data}`);
      break;
    case 'silent_exec_done':
      console.log(`Exit: ${msg.exit_code}, Timed out: ${msg.timed_out}`);
      break;
  }
};
```

### Python (REST)

```python
import requests

response = requests.post('http://localhost:8767/silent-exec',
    json={'id': '1', 'command': 'echo hello'})
result = response.json()
print(result['stdout'])  # hello
```

### Python (WebSocket Streaming)

```python
import json
import websocket

ws = websocket.WebSocket()
ws.connect('ws://localhost:8767/silent-exec-stream')

ws.send(json.dumps({'id': '1', 'command': 'echo hello; sleep 1; echo world'}))

while True:
    msg = json.loads(ws.recv())
    if msg['type'] == 'silent_exec_chunk':
        print(f"[{msg['stream']}] {msg['data']}", end='')
    elif msg['type'] == 'silent_exec_done':
        print(f"\nExit: {msg['exit_code']}")
        break
```

### Kotlin (Android/Termux)

```kotlin
// Using Ktor Client
val client = HttpClient {
    install(JsonFeature) {
        serializer = KotlinxSerializer()
    }
}

val response = client.post<SilentExecResponse>(
    "http://localhost:8767/silent-exec"
) {
    contentType(ContentType.Application.Json)
    body = SilentExecRequest(id = "1", command = "echo hello")
}

println(response.stdout)

// For WebSocket
val webSocket = client.webSocketSession(
    url = "ws://localhost:8767/silent-exec-stream"
)
webSocket.sendMessage(TextContent(json, ContentType.Application.Json))
for (frame in webSocket.incoming) {
    when (frame) {
        is Frame.Text -> println(frame.readText())
    }
}
```

**Data classes:**

```kotlin
@Serializable
data class SilentExecRequest(
    val type: String = "silent_exec",
    val id: String,
    val command: String,
    val cwd: String? = null,
    val env: Map<String, String>? = null,
    @SerialName("timeout_ms") val timeoutMs: Long? = null
)

@Serializable
data class SilentExecResponse(
    val type: String,
    val id: String,
    val success: Boolean,
    @SerialName("exit_code") val exitCode: Int,
    val stdout: String,
    val stderr: String,
    @SerialName("timed_out") val timedOut: Boolean
)
```

**With OkHttp:**

```kotlin
val client = OkHttpClient()
val requestBody = MediaType.parse("application/json")
    ?.let { RequestBody.create(it, """{"id":"1","command":"echo hello"}""") }

val request = Request.Builder()
    .url("http://localhost:8767/silent-exec")
    .post(requestBody!!)
    .build()

val response = client.newCall(request).execute()
println(response.body()?.string())
```
