# dds <-> dsterm contract

FIXLIST #100. The two repos are versioned independently and ship separately:
the Android app updates through Play, the daemon through `dsterm update` or a
package manager. Nothing guarantees they are updated together, so every wire
surface one consumes from the other is a **contract**, not an implementation
detail.

This file is the index and the versioning policy. Each contract is a JSON file
in this directory, pinned by tests in **both** repos:

| Contract | Surface | Pinned by (dsterm) | Pinned by (dds) |
| --- | --- | --- | --- |
| `status_contract.json` | `GET /status` handshake | `status_body_matches_the_shared_contract` | `test/runtime/dsterm_status_contract_test.dart` |
| `ai_contract.json` | 16 `/ai/*` HTTP routes + `generate` WS stream | `contract_parity` | `test/ai/ai_contract_parity_test.dart` |

dds checks this repo out in CI and reads the same files, so a one-sided edit
turns the other repo red (`test.yml` checks out `darkian-studio/dsterm` as
`./dsterm`).

## Protocol version

`GET /status` carries two strings:

- `protocol_version` — the level the daemon speaks.
- `min_client_protocol` — the oldest client level it will serve.

dds advertises its own level in the `X-DS-Protocol` request header; dsterm logs
a warning when that header is newer than the daemon (update dsterm) or older
than `min_client_protocol` (update the app).

Handling is deliberately **fail-open for liveness, fail-closed for features**:
a version mismatch never turns the health probe into a hard gate — a stale
daemon must still answer so the user can be told which side to update — but a
client whose protocol is below `min_client_protocol` must not assume the
surface it wants exists.

## Bump rules

1. **Additive changes do not bump the protocol.** A new optional field in a
   response, a new route, a new capability string: add it to the contract file
   and keep the version. Old clients ignore unknown keys; new clients treat a
   missing key as "the old behaviour".
2. **Any change a shipped client would misread bumps `protocol_version`.**
   Removing or renaming a field, changing a type (`string` vs `number` is the
   classic: see the `protocol_version` note in `status_contract.json`),
   changing a status code's meaning, or changing framing.
3. **`min_client_protocol` follows a breaking bump** once the oldest supported
   app version is known to speak the new level. It may never exceed
   `protocol_version` (asserted in `status_body_matches_the_shared_contract`).
4. **One edit, one contract file.** Do not change the same wire shape in a
   handler and a test without touching the contract; the parity tests are
   generated from the file, not from the handler.

## Not covered yet

The contract files pin the shapes, not the shared *code*. A future step is
generating the Dart/Rust types from one schema; until then the duplication is
real but checked on every push, which is what #74/#100 asked for first.
