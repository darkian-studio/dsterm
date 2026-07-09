# Pairing & Client Approval

DSTerm can print a Shellular-compatible pairing payload so a mobile client can
learn the host id and the end-to-end encryption key out-of-band (via QR). The
key never travels over the network.

## E2E key

A 32-byte `crypto_secretbox` key is generated on first use and stored at
`~/.dsterm/dsterm-<machine-id>.e2ee` with mode `0600`. Override the location
with:

```toml
[security]
key_file = "/home/user/.dsterm/dsterm.e2ee"
```

## dsterm pair

```text
dsterm pair [--host-id <id>] [--no-qr]
```

- Loads (or creates) the E2E key.
- Resolves the host id: `--host-id` wins; otherwise it reads
  `[relay] host_id_file`. If neither is available the command errors.
- Prints a Unicode QR of the payload (unless `--no-qr`) followed by the raw
  payload text.

The payload format is exactly:

```text
<hostId>:<keyBase64>
```

`keyBase64` is the standard-alphabet base64 encoding of the 32-byte key
(44 characters).

## Client approval policy

When a client connects through the relay (later phases), the host decides
whether to admit it based on:

```toml
[security]
unknown_clients = "requires-approval"   # always-allow | always-reject | requires-approval
clients_file = "/home/user/.dsterm/clients.json"   # default: ~/.dsterm/clients.json
```

- `always-allow` — admit unknown clients immediately.
- `always-reject` — refuse unknown clients immediately.
- `requires-approval` (default) — record the client as `Pending` and wait for a
  manual decision.

Known clients are persisted in `clients.json`, keyed by `clientId`, each with
`platform`, `appVersion`, `device`, `firstSeen`, `lastSeen`, and `approval`
(`approved` / `rejected` / `pending`).

## dsterm clients

Manage the approval store offline:

```text
dsterm clients list                 # print every known client and its state
dsterm clients approve <client_id>  # mark a client approved
dsterm clients reject  <client_id>  # mark a client rejected
```

`list` prints `"<clientId>  <Approval>  platform=..  app=.."` per client, or
`No known clients.` when empty. `approve`/`reject` on an unknown id exit with a
non-zero status.
