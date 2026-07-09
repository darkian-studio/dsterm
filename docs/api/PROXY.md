# Localhost Proxy API

**Disabled by default.** Enable with:

```toml
[proxy]
enabled = true
```

Targets are restricted to localhost (`localhost`, `127.0.0.0/8`, `::1`). Any other
host is rejected with HTTP 400. When disabled, every endpoint returns HTTP 403.

## POST /proxy/http

Forwards an HTTP request to a local service.

Body: `{ "url": "http://127.0.0.1:3000/api", "method": "GET", "headers": { }, "body": "..." }`
(`method` defaults to `GET`; `headers` and `body` are optional).

Response:

```json
{ "status": 200, "headers": { "content-type": "application/json" }, "body_base64": "..." }
```

The upstream body is always base64-encoded so binary responses survive transport.

## GET /proxy/ws?url=<ws-url>

Upgrades to a WebSocket and tunnels frames to a local WebSocket service, e.g.
`ws://127.0.0.1:5173/`. Text and binary frames are relayed in both directions.

## Over the relay

The HTTP proxy is reachable remotely via the `http:request` message:

```json
{ "type": "http:request", "id": "r1", "url": "http://127.0.0.1:3000/", "method": "GET" }
```

The host replies with a `result` whose `data` is the `/proxy/http` response above.
WebSocket tunneling is currently local-only (not relay-routed).
