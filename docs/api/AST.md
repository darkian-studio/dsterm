# AST Scope API

DSTerm exposes a single HTTP endpoint that returns the enclosing
syntactic scope chain (functions, classes, methods, interfaces, enums)
at a given cursor line in a source file. The endpoint is backed by
[tree-sitter](https://tree-sitter.github.io/) grammars and is designed
to power editor features like sticky-scroll headers when a Lezer
grammar is not available client-side.

The endpoint is **stateless from the client's perspective** — there is
no `didOpen` / `didClose`. The server maintains a bounded LRU cache
(256 documents) of parsed trees keyed by `document_id`; identical
`version` repeat calls skip parsing entirely.

---

## Endpoint

```text
POST /ast/scope
```

### Request Body

```json
{
  "language": "python",
  "document_id": "file:///workspace/main.py",
  "version": 42,
  "content": "class Foo:\n    def bar(self):\n        pass\n",
  "line": 3
}
```

| Field | Type | Required | Description |
| ----- | ---- | -------- | ----------- |
| `language` | string | Yes | Language ID (see Supported Languages below). |
| `document_id` | string | Yes | Stable identifier for the document (URI recommended). Used as the cache key. |
| `version` | number | Yes | Monotonic integer that increments on every edit. Identical `version` values for the same `document_id` reuse the cached parse tree. |
| `content` | string | Yes | Full UTF-8 source text. |
| `line` | number | Yes | 1-based cursor line at which to compute the enclosing scope chain. |

### Response Body (200 OK)

```json
{
  "scopes": [
    { "name": "Foo", "kind": "class",  "start_line": 1, "end_line": 3 },
    { "name": "bar", "kind": "method", "start_line": 2, "end_line": 3 }
  ]
}
```

| Field | Type | Description |
| ----- | ---- | ----------- |
| `scopes` | array | Ordered from outermost scope (index 0) to innermost. |
| `scopes[].name` | string | Identifier text of the declaration's `name` field. |
| `scopes[].kind` | string | One of `function`, `class`, `method`, `interface`, `enum`. |
| `scopes[].start_line` | number | 1-based line on which the declaration starts. |
| `scopes[].end_line` | number | 1-based line on which the declaration ends. |

Anonymous declarations (arrow functions, function expressions without
a name) do not appear in the output.

### Errors

| Status | Body | Condition |
| ------ | ---- | --------- |
| 400 | `{"error":"unsupported language","language":"<id>"}` | `language` is not in the supported list below. |
| 500 | `{"error":"failed to set language"}` | Internal tree-sitter language registration failed. |
| 500 | `{"error":"parse failed"}` | tree-sitter returned no tree (e.g. cancellation). |

---

## Supported Languages

| `language` field | Underlying grammar |
| ---------------- | ------------------ |
| `python` | `tree-sitter-python` |
| `javascript` | `tree-sitter-javascript` |
| `jsx` | `tree-sitter-javascript` (handles JSX) |
| `typescript` | `tree-sitter-typescript` (TypeScript dialect) |
| `tsx` | `tree-sitter-typescript` (TSX dialect) |

Adding a new language requires:

1. A new arm in `language_for_id` in `src/ast_bridge/languages.rs`.
2. New arms in `node_kind_to_scope_kind` for the language's scope-bearing node kinds.
3. The corresponding `tree-sitter-<lang>` crate in `Cargo.toml`.

---

## Caching

The server keeps an in-process LRU cache of parsed trees:

- **Capacity:** 256 documents.
- **Key:** `document_id`.
- **Value:** `{version, source_bytes, tree}`.
- **Hit:** same `document_id` *and* same `version` → the cached tree
  is walked directly, no parsing.
- **Miss / version change:** full reparse from scratch; cache entry
  is replaced.

Clients should send a stable `document_id` for each open file and
increment `version` on every edit. There is no explicit close — old
entries are evicted by LRU pressure.

---

## Example

```bash
curl -X POST http://localhost:8767/ast/scope \
  -H "Content-Type: application/json" \
  -d '{
    "language": "python",
    "document_id": "file:///tmp/example.py",
    "version": 1,
    "content": "class Foo:\n    def bar(self):\n        pass\n",
    "line": 3
  }'
```

Response:

```json
{"scopes":[
  {"name":"Foo","kind":"class","start_line":1,"end_line":3},
  {"name":"bar","kind":"method","start_line":2,"end_line":3}
]}
```
