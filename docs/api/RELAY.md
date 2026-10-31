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
- `dsterm startup` — installs an autostart entry that runs `dsterm host`
  (relay host). It prompts before adding `--remote` (the zero-config
  `/fs/*` API) to the entry — declining, or piping stdin non-interactively,
  installs without it — and `--with-filesystem` opts in without the prompt.
  Uses a Termux:Boot script on Android, a systemd user unit on Linux, a
  launchd LaunchAgent on macOS, and a per-user Startup-folder script on
  Windows.
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

Since #72 the inner plaintext may itself be wrapped (v2 clients):

```json
{ "clientId": "…", "msg": { … } }
```

- The outer `clientId` is a relay-supplied routing label and carries no
  proof; the v2 wrapper binds the id *inside* the AEAD payload too. A frame
  whose inner id disagrees with the outer label is dropped (a relay that
  re-labelled someone else's ciphertext).
- The host echoes the client's own shape back: a client that first speaks
  the wrapper gets wrapped replies, everything else stays bare (legacy
  Shellular parsers read the bare message and would drop a wrapper).
- The sealing key is resolved per client, in order: the client's pinned
  `identityKey` from `clients.json` (a frame under any other key —
  including the shared master — is dropped once pinned); else a matching
  `pendingPairings` entry (first such frame binds it); else the shared
  master (legacy pairings from before #72). See `PAIRING.md`.

The inner plaintext (after unwrapping) is a JSON message. Client→host (`IncomingMsg`) includes
`ping`, `terminal:create|data|resize|close|list|attach`,
`fs:read|write|mkdir|delete|rename|stat|list`, `project:file-search`,
`sysmon:get|subscribe|unsubscribe`, `ports:list|kill`, `exec`, `http:request`,
`agents:start|input|kill`, and `ws:open|data|close`.
Host→client (`OutgoingMsg`) includes `pong`, `error`, `result { respTo, data }`,
`terminal:data`, `terminal:event`, `agent:output`, `agent:exit`, `ws:data`,
`ws:close`, and `sysmon:update`.

Plaintext is only accepted for the control allowlist: `ping`, `pong`, and
`session:*`.
