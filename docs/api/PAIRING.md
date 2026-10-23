# Pairing & Client Approval

DSTerm can print a Shellular-compatible pairing payload so a mobile client can
learn the host id and an end-to-end encryption key out-of-band (via QR). The
key never travels over the network; every `dsterm pair` mints a fresh one.

## E2E key vs pairing keys

A 32-byte `crypto_secretbox` **shared master key** is generated on first use
and stored at `~/.dsterm/dsterm-<machine-id>.e2ee` with mode `0600`. Override
the location with:

```toml
[security]
key_file = "/home/user/.dsterm/dsterm.e2ee"
```

Since #72 the QR no longer carries this master. Each `dsterm pair` mints a
fresh 32-byte **pairing key**, recorded in `clients.json` under
`pendingPairings` (FIFO-capped at 8) until the scanner's first approved frame
binds it to that client's `identityKey`. The master remains the fallback for
clients paired before #72 — and only until they are paired again: a pinned
client sealed with the master is dropped.

## dsterm pair

```text
dsterm pair [--host-id <id>] [--no-qr]
```

- Loads (or creates) the shared E2E key (it must exist for the legacy
  fallback even though the QR does not carry it).
- Mints a fresh pairing key and records it in `clients.json` *before*
  printing, so a printed payload is always redeemable.
- Resolves the host id: `--host-id` wins; otherwise it reads
  `[relay] host_id_file`. If neither is available the command errors.
- Prints a Unicode QR of the payload (unless `--no-qr`) followed by the raw
  payload text.

The payload format is exactly:

```text
<hostId>:<keyBase64>
```

`keyBase64` is the standard-alphabet base64 encoding of the 32-byte pairing
key (44 characters) — same shape as a pre-#72 payload, but never the shared
master again.

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
(`approved` / `rejected` / `pending`). A client bound at first use also gets
`identityKey` + `identityBoundAt`; keys still waiting for their first frame
sit in `pendingPairings`. The file is owner-only (`0600`) on unix.

## dsterm clients

Manage the approval store offline:

```text
dsterm clients list                 # print every known client and its state
dsterm clients approve <client_id>  # mark a client approved
dsterm clients reject  <client_id>  # mark a client rejected
dsterm clients unpin   <client_id>  # drop the pinned key (re-pair to rebind)
```

`list` prints `"<clientId>  <Approval>  platform=..  app=..  key=per-client|shared"`
per client (plus a count of pairing keys awaiting first use), or
`No known clients.` when empty. `approve`/`reject`/`unpin` on an unknown id
exit with a non-zero status. `unpin` takes effect on the client's next
join/reconnect.
