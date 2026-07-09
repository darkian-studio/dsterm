use crate::terminal::get_config;
use axum::{response::IntoResponse, Json};
use serde::{Deserialize, Serialize};
use std::{
    collections::{HashMap, HashSet},
    fs,
};
use sysinfo::{Pid, System};

#[derive(Debug, Serialize)]
struct PortEntry {
    port: u16,
    protocol: String,
    pid: Option<u32>,
    process: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct KillPortRequest {
    port: u16,
}

fn parse_proc_net(path: &str, protocol: &str) -> Vec<(u16, u64, String)> {
    let Ok(content) = fs::read_to_string(path) else {
        return Vec::new();
    };
    content
        .lines()
        .skip(1)
        .filter_map(|line| {
            let parts = line.split_whitespace().collect::<Vec<_>>();
            let local = *parts.get(1)?;
            let state = *parts.get(3)?;
            if protocol == "tcp" && state != "0A" {
                return None;
            }
            let port_hex = local.rsplit_once(':')?.1;
            let port = u16::from_str_radix(port_hex, 16).ok()?;
            let inode = parts.get(9)?.parse::<u64>().ok()?;
            Some((port, inode, protocol.to_string()))
        })
        .collect()
}

fn inode_to_pid() -> HashMap<u64, u32> {
    let mut map = HashMap::new();
    let Ok(proc_entries) = fs::read_dir("/proc") else {
        return map;
    };
    for proc_entry in proc_entries.flatten() {
        let Ok(pid) = proc_entry.file_name().to_string_lossy().parse::<u32>() else {
            continue;
        };
        let fd_dir = proc_entry.path().join("fd");
        let Ok(fd_entries) = fs::read_dir(fd_dir) else {
            continue;
        };
        for fd in fd_entries.flatten() {
            let Ok(target) = fs::read_link(fd.path()) else {
                continue;
            };
            let target = target.to_string_lossy();
            if let Some(inode) = target
                .strip_prefix("socket:[")
                .and_then(|s| s.strip_suffix(']'))
            {
                if let Ok(inode) = inode.parse::<u64>() {
                    map.entry(inode).or_insert(pid);
                }
            }
        }
    }
    map
}

pub async fn list_ports() -> impl IntoResponse {
    let sockets = [
        ("/proc/net/tcp", "tcp"),
        ("/proc/net/tcp6", "tcp"),
        ("/proc/net/udp", "udp"),
        ("/proc/net/udp6", "udp"),
    ]
    .into_iter()
    .flat_map(|(path, protocol)| parse_proc_net(path, protocol))
    .collect::<Vec<_>>();
    let inode_pid = inode_to_pid();
    let mut system = System::new_all();
    system.refresh_all();
    let mut seen = HashSet::new();
    let mut ports = Vec::new();
    for (port, inode, protocol) in sockets {
        let pid = inode_pid.get(&inode).copied();
        if !seen.insert((port, pid, protocol.clone())) {
            continue;
        }
        let process = pid.and_then(|pid| {
            system
                .process(Pid::from_u32(pid))
                .map(|process| process.name().to_string_lossy().into_owned())
        });
        ports.push(PortEntry {
            port,
            protocol,
            pid,
            process,
        });
    }
    ports.sort_by_key(|entry| entry.port);
    Json(serde_json::json!({ "ports": ports })).into_response()
}

pub async fn kill_port(Json(req): Json<KillPortRequest>) -> impl IntoResponse {
    if !get_config().ports.kill_enabled {
        return (
            axum::http::StatusCode::FORBIDDEN,
            Json(serde_json::json!({ "error": "Port killing is disabled" })),
        )
            .into_response();
    }
    if req.port == 0 {
        return (
            axum::http::StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": "Invalid port" })),
        )
            .into_response();
    }
    let sockets = [("/proc/net/tcp", "tcp"), ("/proc/net/tcp6", "tcp")]
        .into_iter()
        .flat_map(|(path, protocol)| parse_proc_net(path, protocol))
        .filter(|(port, _, _)| *port == req.port)
        .collect::<Vec<_>>();
    let inode_pid = inode_to_pid();
    let pids = sockets
        .iter()
        .filter_map(|(_, inode, _)| inode_pid.get(inode).copied())
        .collect::<HashSet<_>>();
    let mut killed = Vec::new();
    for pid in pids {
        let result = unsafe { libc::kill(pid as i32, libc::SIGKILL) };
        if result == 0 {
            killed.push(pid);
        }
    }
    Json(serde_json::json!({ "success": true, "killed": killed })).into_response()
}

pub fn ports_routes() -> axum::Router {
    axum::Router::new()
        .route("/ports", axum::routing::get(list_ports))
        .route("/ports/kill", axum::routing::post(kill_port))
}
