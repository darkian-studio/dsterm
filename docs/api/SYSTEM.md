# System Monitor & Ports API

## GET /sysmon

Returns a point-in-time system snapshot. Always available (no config gate).

```json
{
  "cpu": { "brand": "...", "cores": 8, "usage_percent": 12.5 },
  "memory": { "total": 0, "used": 0, "available": 0 },
  "uptime_secs": 0,
  "disks": [ { "name": "...", "mount_point": "/", "total": 0, "available": 0 } ],
  "battery": { "percent": 87, "status": "Discharging" }
}
```

`battery` is read from `/sys/class/power_supply/*` (first entry whose
`type` is `Battery`). It is `null` when no battery is present or the sysfs
node is unreadable (common on Termux). `status` is the raw sysfs value, e.g.
`Charging`, `Discharging`, `Full`, or `Unknown`.

## GET /ports

Lists listening TCP sockets and bound UDP sockets by parsing `/proc/net/{tcp,tcp6,udp,udp6}`
and mapping socket inodes to PIDs via `/proc/<pid>/fd`.

```json
{
  "ports": [ { "port": 3000, "protocol": "tcp", "pid": 4242, "process": "node" } ]
}
```

`pid`/`process` may be `null` when the owning process cannot be resolved.

## POST /ports/kill

**Disabled by default.** Enable with:

```toml
[ports]
kill_enabled = true
```

Body: `{ "port": 3000 }`. Sends `SIGKILL` to every process owning that port.

```json
{ "success": true, "killed": [4242] }
```

Returns HTTP 403 (`{"error":"Port killing is disabled"}`) when
`kill_enabled = false`, or HTTP 400 (`{"error":"Invalid port"}`) for port `0`.
