# dsterm API Reference

## Base URL

```
http://localhost:8767
```

## REST Endpoints

### Silent Command Execution

Execute a command without a PTY and get the result.

**Endpoint:** `POST /silent-exec`

**Request Body:**
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
| `id` | string | Yes | Unique request identifier |
| `command` | string | Yes | Command to execute |
| `cwd` | string | No | Working directory for the command |
| `env` | object | No | Environment variables to set |
| `timeout_ms` | number | No | Timeout in milliseconds (default: 30000) |

**Response:**
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
| `type` | string | Message type identifier |
| `id` | string | Request identifier |
| `success` | boolean | True if exit code is 0 |
| `exit_code` | number | Process exit code (-1 on error) |
| `stdout` | string | Standard output from command |
| `stderr` | string | Standard error from command |
| `timed_out` | boolean | True if command timed out |

---

## WebSocket Endpoints

### Silent Command Execution (Streaming)

Execute a command and receive output as a stream.

**Endpoint:** `GET /silent-exec-stream`

**Connection:** Standard WebSocket connection

**Send (same as REST request):**
```json
{
  "type": "silent_exec",
  "id": "unique-request-id",
  "command": "ls -la",
  "cwd": "/data/data/com.termux/files/home",
  "timeout_ms": 60000
}
```

**Receive - Chunk (stdout):**
```json
{
  "type": "silent_exec_chunk",
  "id": "unique-request-id",
  "stream": "stdout",
  "data": "total 12\n"
}
```

**Receive - Chunk (stderr):**
```json
{
  "type": "silent_exec_chunk",
  "id": "unique-request-id",
  "stream": "stderr",
  "data": "error message\n"
}
```

**Receive - Done:**
```json
{
  "type": "silent_exec_done",
  "id": "unique-request-id",
  "exit_code": 0,
  "timed_out": false
}
```

---

## Error Responses

### 400 Bad Request
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

### 500 Internal Server Error
```json
{
  "type": "silent_exec_result",
  "id": "request-id",
  "success": false,
  "exit_code": -1,
  "stdout": "",
  "stderr": "Failed to spawn command: ...",
  "timed_out": false
}
```

---

## Examples

### cURL (REST)

```bash
curl -X POST http://localhost:8767/silent-exec \
  -H "Content-Type: application/json" \
  -d '{"id": "1", "command": "echo hello"}'
```

### JavaScript (WebSocket)

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
  const data = JSON.parse(event.data);
  console.log(data);
};
```

### Python

```python
import requests
import json

response = requests.post(
    'http://localhost:8767/silent-exec',
    json={
        'id': '1',
        'command': 'echo hello'
    }
)

result = response.json()
print(result['stdout'])  # hello
```

### Kotlin (Android/Termux)

```kotlin
// Using Ktor Client (Android/Termux)
val client = HttpClient {
    install(JsonFeature) {
        serializer = KotlinxSerializer()
    }
}

val request = SilentExecRequest(
    id = "1",
    command = "echo hello",
    timeoutMs = 30000
)

val response = client.post<SilentExecResponse>(
    "http://localhost:8767/silent-exec"
) {
    contentType(ContentType.Application.Json)
    body = request
}

println(response.stdout)  // hello

// For WebSocket
val webSocket = client.webSocketSession(
    url = "ws://localhost:8767/silent-exec-stream"
)

// Send request
webSocket.sendMessage(TextContent(json, ContentType.Application.Json))

// Receive messages
for (frame in webSocket.incoming) {
    when (frame) {
        is Frame.Text -> println(frame.readText())
    }
}
```

**Data Classes:**
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

**With OkHttp (simpler):**
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