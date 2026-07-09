# Relay Host Mode

DSTerm can connect **outbound** to a Shellular-compatible relay so an approved
mobile/web client can reach this machine from anywhere. All feature traffic is
end-to-end encrypted; the relay only routes opaque frames.

## Configuration

```toml
[relay]
server_url = "https://relay.example.com:3000"
# host_id_file = "/home/user/.dsterm/host_id"
heartbeat_secs = 25
reconnect_secs = [1, 2, 5, 10, 30]

[security]
unknown_clients = "requires-approval"   # always-allow | always-reject | requires-approval
```

## Commands

- `dsterm register` — `POST {server_url}/host/register` with `{machineId, platform}`,
  caches the returned `hostId` (default `~/.dsterm/host_id`).
- `dsterm host` — loads/creates the E2E key, resolves the `hostId` (registering
  if needed), serves the local API on `127.0.0.1:<port>`, and runs the relay
  client. Exits on Ctrl-C.
- `dsterm startup` — installs an autostart entry (systemd user unit on Linux,
  Termux:Boot script on Android) that runs `dsterm host`.
- `dsterm pair` / `dsterm clients` — see `docs/api/PAIRING.md`.

## Handshake & lifecycle

```
host  --WS-->  {ws server_url}/cli?hostId=<id>
host  --send-> session:host { hostId, machineId, platform }
relay --recv-> session:hosted { sessionId }
```

- Heartbeat: the host sends `{"type":"ping"}` every `heartbeat_secs`.
- Reconnect: on drop, the host retries using the `reconnect_secs` ladder
  (resets after a successful connection).
- `session:error` ends the connection and triggers a reconnect.

## Client approval

On `session:client-join { clientId, clientInfo? }` the host consults the client
store (`clients.json`) using the `unknown_clients` policy and replies:

```json
{ "type": "session:client-approve", "clientId": "…", "approved": true }
```

Encrypted messages from clients that are not approved are dropped. Manage the
store with `dsterm clients list|approve|reject`.

## Encrypted envelope

Every feature message is wrapped as:

```json
{ "type": "encrypted", "clientId": "…", "nonce": "<base64>", "ciphertext": "<base64>" }
```

The inner plaintext is a JSON message. Client→host (`IncomingMsg`) includes
`ping`, `terminal:create|data|resize|close|list`, `fs:read|write|mkdir|delete|rename|stat`,
`project:file-search`, `sysmon:get`, `ports:list|kill`, `exec`, and `http:request`.
Host→client (`OutgoingMsg`) includes `pong`, `error`, `terminal:data`,
`terminal:event`, and `result { respTo, data }`.

Plaintext is only accepted for the control allowlist: `ping`, `pong`, and
`session:*`.
