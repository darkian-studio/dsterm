# Transfer

Standalone file/directory transfer between two `dsterm` instances. No DS
knowledge on either side: the sender dials a plain `host:port`, the receiver
writes under its own `--dest`.

## Receiver

```bash
dsterm --listen-transfer
dsterm --listen-transfer -p 8770
dsterm --listen-transfer --dest ./incoming
dsterm --listen-transfer --auto-receive -p 8770
dsterm --self-update --listen-transfer --auto-receive -p 8770
```

`--auto-receive` modifies `--listen-transfer`; alone it is a usage error.
Automatic mode accepts loopback senders by default — loopback bounds origin,
not local user identity, so remote senders need the explicit
`--allow-remote` opt-in. The unattended default directory is
`~/.dsterm/incoming/`, never the service working directory.

Conflicting destinations reject by default; `--overwrite` replaces after the
new payload verifies, `--rename` writes `name (1)` alongside instead.

## Sender

```bash
dsterm transfer ./foo 192.168.1.42:8770
dsterm transfer ./foo 192.168.1.42
dsterm transfer ./foo '[::1]:8770' --as renamed
```

A bare host uses the default transfer port (7773). IPv6 literals must be
bracketed: unbracketed `::1:8770` is rejected rather than guessed at.

## Guarantees

- Payloads stream in bounded buffers with an incremental sha256. The digest
  catches corruption, not tampering — pair non-loopback use with a PAKE or
  pinned TLS, never the bare digest alone.
- Transfers land in a temp sibling and rename into place only after hash and
  metadata checks pass; failures never read as success.
- While busy (confirming or transferring) the listener rejects with `busy`
  instead of queuing.
- Hardlinks arrive as independent files; sockets/FIFOs/devices are skipped
  with a warning; dangling symlinks are recreated as dangling.
