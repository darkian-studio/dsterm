# Shellular Integration Tracking

This document tracks implementation against `.tmp/shellular-dsterm.md`.
It is the working source of truth for scope, order, and status.

## Status Legend

- `todo`: not started
- `in-progress`: code exists but is incomplete or unverified
- `done`: implemented and verified by CI
- `blocked`: waiting on a decision or external dependency

## Ground Rules

- Direct DSTerm HTTP/WebSocket mode remains first-class.
- Relay mode is additive and must share handler cores with direct mode.
- Remote capabilities must be behind pairing, encryption, and client approval before they are exposed through the relay.
- Build, lint, and test verification happens through GitHub CI when we intentionally push.
- Do not push solely to test until the current implementation slice is coherent.

## Implementation Order

| Step | Status | Scope | Notes |
| --- | --- | --- | --- |
| 1 | in-progress | Protocol + handler-core foundation | `src/protocol/` scaffold exists. Terminal fan-out work started. Full dispatcher and transport-agnostic cores still needed. |
| 2 | in-progress | Secretbox encryption + key management | `src/relay/crypto.rs` added with XSalsa20-Poly1305, base64 envelope parts, key-file load/create, and Unix `0600` create mode. CI verification pending. |
| 3 | todo | Relay transport spine | `POST /host/register`, `/cli?hostId`, heartbeat, reconnect, encrypted send/receive. |
| 4 | todo | Pairing + client approval | QR payload, clients JSON store, unknown-client policy, `dsterm clients`. |
| 5 | in-progress | Terminal over relay prerequisites | Logical `terminalId` added to sessions/listing. Multi-client broadcast fan-out started. Relay adapters not yet added. |
| 6 | todo | Command execution over relay | Reuse existing exec cores, then enforce workspace-root policy for remote exposure. |
| 7 | in-progress | Filesystem + project search | HTTP handlers added for read/write/mkdir/delete/rename/stat/search with workspace bounds. Git/status integration still todo. |
| 8 | in-progress | Sysmon + ports | Native `/sysmon`, `/ports`, `/ports/kill` surfaces added. CI verification pending. Battery and richer metrics still todo. |
| 9 | todo | Localhost proxy | HTTP and WS localhost-only tunnel, binary path, backpressure. |
| 10 | todo | ACP agents | Reuse extension-host bridge pattern; add ACP JSON-RPC and permission mediation. |
| 11 | todo | Daemon/startup | OS-native supervision docs/templates; no PM2. |
| 12 | todo | Docs + compatibility matrix | Relay, pairing, filesystem, proxy, agents docs and protocol type matrix. |

## Current Working Slice

The current unverified working slice is:

- Add protocol message/envelope scaffolding in `src/protocol/`.
- Add config sections for relay, security, filesystem, and proxy.
- Add direct HTTP filesystem routes bounded to `filesystem.workspace_root`, disabled by default.
- Add direct HTTP sysmon and ports routes; port killing is disabled by default.
- Replace terminal single-attach output channels with broadcast fan-out.
- Add `terminalId` metadata while preserving the existing `/terminals` PID response body.
- Add relay secretbox encryption/key-management foundation.

Before pushing for CI, finish or review:

- Ensure all new routes are intended to be direct-mode public surfaces.
- Review the default-off filesystem and port-kill gates before exposing relay equivalents.
- Add dispatcher functions or defer dispatcher wiring explicitly to the next slice.
- Recheck Linux/Android assumptions for `/proc`-based ports and `sysinfo` APIs.

## Security Checklist

| Item | Status | Notes |
| --- | --- | --- |
| E2E encrypted relay messages | todo | Required before relay feature dispatch. |
| Key file permissions | in-progress | Implemented for newly-created Unix key files in `src/relay/crypto.rs`; CI verification pending. |
| Pairing QR | todo | Payload remains `hostId:keyBase64`. |
| Client approval gate | todo | Must gate all relay dispatch. |
| Workspace root enforcement | in-progress | Implemented for filesystem HTTP handlers when enabled; exec bounding still todo. |
| Root delete guard | in-progress | Implemented for filesystem delete. |
| Remote exec blast-radius review | todo | Required before exposing exec over relay. |
| Localhost-only proxy validation | todo | Required before proxy implementation. |

## Verification

Local build/lint/test is not required for this workflow. When an implementation slice is ready:

1. Commit the coherent slice.
2. Push.
3. Use GitHub CI results as the build/lint/test signal.
4. Fix CI failures in follow-up commits.

## Open Decisions

- Should new direct-mode filesystem routes remain disabled by default after relay security lands?
- Should `/ports/kill` remain disabled by default even in direct mode?
- Should terminal logical IDs become route keys later, or remain metadata while PID routes stay for compatibility?
- Which relay server URL default should ship for real use: localhost self-host default or a public Shellular-compatible relay?
