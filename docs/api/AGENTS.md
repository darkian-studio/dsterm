# ACP Agent Bridge API

Runs an AI coding agent (Claude Code, Codex, OpenCode, Copilot, etc.) as a
subprocess and bridges its Agent Client Protocol (ACP) stdio to a WebSocket.
The wire protocol is **newline-delimited JSON** (one JSON object per line), the
same framing used by the extension-host bridge. ACP permission requests
(`session/request_permission`) flow through to the client verbatim; the client's
reply is written back to the agent's stdin.

Like the LSP/DAP/MCP/extension-host bridges, these are local endpoints.

## POST /agents/start

Body: `{ "id": "a1", "command": "npx", "args": ["-y","@zed-industries/codex-acp"], "cwd": "/proj", "env": { } }`
(`args`, `cwd`, `env` optional). Returns:

```json
{ "id": "a1", "ws_path": "/agents/a1" }
```

HTTP 409 if the id already exists; HTTP 500 if the process fails to spawn.

## GET /agents/{id}

WebSocket. Each text frame sent by the client is written to the agent's stdin
followed by a newline; each line the agent prints to stdout is sent back as a
text frame. Closing the socket terminates the agent.

## POST /agents/kill

Body: `{ "id": "a1" }` to kill one session, or `{}` to kill all. Returns
`{ "killed": ["a1"] }`. The grace period before force-kill is
`[bridges] kill_timeout_secs`.

## Over the relay

Agents are reachable remotely through the encrypted relay:

- `agents:start { id?, command, args?, cwd?, env? }` spawns an agent and replies
  with `result { respTo, data: { agentId } }`.
- `agents:input { agentId, data }` writes one NDJSON line to the agent's stdin.
- `agents:kill { agentId }` terminates it.

The host streams each stdout line back as `agent:output { agentId, data }` and
emits `agent:exit { agentId }` when the agent's stdout closes. The client drives
the ACP JSON-RPC conversation end-to-end over this pipe.
