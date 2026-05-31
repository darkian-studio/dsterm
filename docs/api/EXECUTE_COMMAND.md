# Execute Command API

Run a shell command via a PTY and retrieve the (ANSI-stripped) output.

```text
POST /execute-command
```

Spawns `sh -c <command>` inside a PTY, waits up to 30 seconds for completion, and returns the output with ANSI escape codes stripped.

## Request Body

```json
{
  "command": "ls -la /tmp",
  "cwd": "/data/data/com.termux/files/home"
}
```

| Field | Type | Required | Description |
| ------- | ------ | ---------- | ------------- |
| `command` | string | Yes | Shell command to execute |
| `cwd` | string | No | Working directory. If empty or omitted, defaults to `$HOME` |
| `u_cwd` | string | No | Alias for `cwd` (legacy field, same behavior) |

If both `cwd` and `u_cwd` are provided, `cwd` takes precedence.

## Response

```json
{
  "output": "total 12\ndrwxr-xr-x 2 user user 4096 Jan 1 12:00 .\ndrwxr-xr-x 4 user user 4096 Jan 1 11:00 ..\n",
  "error": null
}
```

| Field | Type | Description |
| ------- | ------ | ------------- |
| `output` | string | Command output with ANSI escape sequences removed |
| `error` | string or null | Error message if the command failed |

## Errors

```json
{
  "output": "",
  "error": "Working directory does not exist"
}
```

```json
{
  "output": "",
  "error": "Command execution timed out"
}
```

| Status | Condition |
| -------- | ----------- |
| 400 | Working directory does not exist |
| 500 | Execution timed out (>30s), spawn failed, or internal error |

## Notes

- The command runs as `sh -c <command>` — shell syntax (pipes, redirects, variables) is supported.
- A **hard 30-second timeout** is applied. If the command exceeds this, the process is killed.
- ANSI escape sequences are stripped from the output via regex. This includes color codes (`\x1B[...m`), cursor movement, and erase-in-display sequences.
- The PTY is created at a fixed size of 80×24.

## Examples

### cURL

```bash
curl -X POST http://localhost:8767/execute-command \
  -H "Content-Type: application/json" \
  -d '{"command": "echo hello world"}'
```

### Python

```python
import requests

res = requests.post('http://localhost:8767/execute-command', json={
    'command': 'uname -a',
    'cwd': '/tmp'
})
print(res.json()['output'])
```

### JavaScript

```javascript
const res = await fetch('http://localhost:8767/execute-command', {
  method: 'POST',
  headers: { 'Content-Type': 'application/json' },
  body: JSON.stringify({ command: 'whoami' })
});
const data = await res.json();
console.log(data.output);
```
