# Filesystem API

All filesystem endpoints are **disabled by default**. Enable them and set the
sandbox root in your config:

```toml
[filesystem]
enabled = true
workspace_root = "/home/user/project"
max_read_bytes = 2097152
```

Every `path` is resolved against `workspace_root` and lexically normalized.
Requests that escape the root are rejected with HTTP 400
(`{"error":"Path escapes workspace root"}`). When `enabled = false`, every
endpoint returns HTTP 403 (`{"error":"Filesystem API is disabled"}`).

## GET /fs/read?path=<relative>

Reads a file. Rejects files larger than `max_read_bytes` (HTTP 413).

Response:

```json
{ "path": "src/main.rs", "encoding": "utf-8", "content": "..." }
```

`encoding` is `"base64"` when the file contains NUL bytes, otherwise `"utf-8"`.

## POST /fs/write

Body: `{ "path": "notes.txt", "content": "...", "encoding": "utf-8" }`
(`encoding` may be `"utf-8"` (default) or `"base64"`). Parent directories are
created automatically. Response: `{ "success": true }`.

## POST /fs/mkdir

Body: `{ "path": "a/b/c" }`. Creates directories recursively. Response:
`{ "success": true }`.

## POST /fs/delete

Body: `{ "path": "build", "recursive": true }`. Refuses to delete the workspace
root (HTTP 400). Response: `{ "success": true }`.

## POST /fs/rename

Body: `{ "from": "old.txt", "to": "new.txt" }`. Response: `{ "success": true }`.

## GET /fs/stat?path=<relative>

Response:

```json
{ "path": "src", "is_dir": true, "is_file": false, "len": 4096, "modified": 1720000000 }
```

## GET /fs/git/status

Runs `git -C <workspace_root>` and returns the current branch and porcelain
change list. Requires `git` on `PATH` and a repo at the root (otherwise HTTP 500).

```json
{
  "branch": "main",
  "files": [ { "status": "M", "path": "src/main.rs" }, { "status": "??", "path": "new.txt" } ]
}
```

## GET /project/file-search?query=<text>&limit=<n>

Case-insensitive filename search under the root (skips `.git` and `target`).
`limit` defaults to 100 and is capped at 1000.

```json
{ "results": [ "src/main.rs", "src/fs.rs" ] }
```
