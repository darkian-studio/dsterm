use crate::terminal::get_config;
use axum::{response::IntoResponse, Json};
use serde::{Deserialize, Serialize};
use sysinfo::{Pid, System};

#[cfg(windows)]
use std::collections::HashSet;
#[cfg(unix)]
use std::{
    collections::{HashMap, HashSet},
    fs,
};

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

#[cfg(unix)]
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

#[cfg(unix)]
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

#[cfg(unix)]
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

#[cfg(unix)]
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

#[cfg(windows)]
#[derive(Debug)]
struct WindowsSocket {
    port: u16,
    pid: u32,
    protocol: &'static str,
}

#[cfg(windows)]
fn windows_rows_from_buffer<T: Copy>(buffer: &[u8]) -> Vec<T> {
    use std::{mem, ptr};

    let row_size = mem::size_of::<T>();
    if row_size == 0 || buffer.len() < mem::size_of::<u32>() {
        return Vec::new();
    }
    let count = unsafe { ptr::read_unaligned(buffer.as_ptr().cast::<u32>()) } as usize;
    let rows_available = (buffer.len() - mem::size_of::<u32>()) / row_size;
    let count = count.min(rows_available);
    let first_row = unsafe { buffer.as_ptr().add(mem::size_of::<u32>()) };

    (0..count)
        .map(|index| unsafe { ptr::read_unaligned(first_row.add(index * row_size).cast::<T>()) })
        .collect()
}

#[cfg(windows)]
fn windows_table_rows<T: Copy>(
    mut get_table: impl FnMut(*mut std::ffi::c_void, *mut u32) -> u32,
) -> Vec<T> {
    use std::{mem, ptr};
    use windows_sys::Win32::Foundation::ERROR_INSUFFICIENT_BUFFER;

    let mut byte_len = 0u32;
    let first_status = get_table(ptr::null_mut(), &mut byte_len);
    if first_status != ERROR_INSUFFICIENT_BUFFER || byte_len < mem::size_of::<u32>() as u32 {
        return Vec::new();
    }

    // The table can grow between calls, so retry once with the larger size.
    for _ in 0..2 {
        let mut buffer = vec![0u8; byte_len as usize];
        let status = get_table(buffer.as_mut_ptr().cast(), &mut byte_len);
        if status == 0 {
            return windows_rows_from_buffer(&buffer);
        }
        if status != ERROR_INSUFFICIENT_BUFFER || byte_len <= buffer.len() as u32 {
            return Vec::new();
        }
    }

    Vec::new()
}

#[cfg(windows)]
fn windows_tcp_listeners() -> Vec<WindowsSocket> {
    use windows_sys::Win32::{
        NetworkManagement::IpHelper::{
            GetExtendedTcpTable, MIB_TCP6ROW_OWNER_PID, MIB_TCPROW_OWNER_PID,
            TCP_TABLE_OWNER_PID_LISTENER,
        },
        Networking::WinSock::{AF_INET, AF_INET6},
    };

    let ipv4 = windows_table_rows::<MIB_TCPROW_OWNER_PID>(|table, size| unsafe {
        GetExtendedTcpTable(
            table,
            size,
            0,
            AF_INET as u32,
            TCP_TABLE_OWNER_PID_LISTENER,
            0,
        )
    });
    let ipv6 = windows_table_rows::<MIB_TCP6ROW_OWNER_PID>(|table, size| unsafe {
        GetExtendedTcpTable(
            table,
            size,
            0,
            AF_INET6 as u32,
            TCP_TABLE_OWNER_PID_LISTENER,
            0,
        )
    });

    ipv4.into_iter()
        .map(|row| WindowsSocket {
            port: u16::from_be(row.dwLocalPort as u16),
            pid: row.dwOwningPid,
            protocol: "tcp",
        })
        .chain(ipv6.into_iter().map(|row| WindowsSocket {
            port: u16::from_be(row.dwLocalPort as u16),
            pid: row.dwOwningPid,
            protocol: "tcp",
        }))
        .collect()
}

#[cfg(windows)]
fn windows_udp_bindings() -> Vec<WindowsSocket> {
    use windows_sys::Win32::{
        NetworkManagement::IpHelper::{
            GetExtendedUdpTable, MIB_UDP6ROW_OWNER_PID, MIB_UDPROW_OWNER_PID, UDP_TABLE_OWNER_PID,
        },
        Networking::WinSock::{AF_INET, AF_INET6},
    };

    let ipv4 = windows_table_rows::<MIB_UDPROW_OWNER_PID>(|table, size| unsafe {
        GetExtendedUdpTable(table, size, 0, AF_INET as u32, UDP_TABLE_OWNER_PID, 0)
    });
    let ipv6 = windows_table_rows::<MIB_UDP6ROW_OWNER_PID>(|table, size| unsafe {
        GetExtendedUdpTable(table, size, 0, AF_INET6 as u32, UDP_TABLE_OWNER_PID, 0)
    });

    ipv4.into_iter()
        .map(|row| WindowsSocket {
            port: u16::from_be(row.dwLocalPort as u16),
            pid: row.dwOwningPid,
            protocol: "udp",
        })
        .chain(ipv6.into_iter().map(|row| WindowsSocket {
            port: u16::from_be(row.dwLocalPort as u16),
            pid: row.dwOwningPid,
            protocol: "udp",
        }))
        .collect()
}

#[cfg(windows)]
pub async fn list_ports() -> impl IntoResponse {
    let sockets = windows_tcp_listeners()
        .into_iter()
        .chain(windows_udp_bindings())
        .collect::<Vec<_>>();
    let mut system = System::new_all();
    system.refresh_all();
    let mut seen = HashSet::new();
    let mut ports = Vec::new();
    for socket in sockets {
        if !seen.insert((socket.port, socket.pid, socket.protocol)) {
            continue;
        }
        let process = system
            .process(Pid::from_u32(socket.pid))
            .map(|process| process.name().to_string_lossy().into_owned());
        ports.push(PortEntry {
            port: socket.port,
            protocol: socket.protocol.to_string(),
            pid: Some(socket.pid),
            process,
        });
    }
    ports.sort_by_key(|entry| entry.port);
    Json(serde_json::json!({ "ports": ports })).into_response()
}

#[cfg(windows)]
fn terminate_windows_process(pid: u32) -> bool {
    use windows_sys::Win32::{
        Foundation::CloseHandle,
        System::Threading::{OpenProcess, TerminateProcess, PROCESS_TERMINATE},
    };

    unsafe {
        let process = OpenProcess(PROCESS_TERMINATE, 0, pid);
        if process.is_null() {
            return false;
        }
        let terminated = TerminateProcess(process, 1) != 0;
        let _ = CloseHandle(process);
        terminated
    }
}

#[cfg(windows)]
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

    let pids = windows_tcp_listeners()
        .into_iter()
        .filter(|socket| socket.port == req.port)
        .map(|socket| socket.pid)
        .collect::<HashSet<_>>();
    let killed = pids
        .into_iter()
        .filter(|pid| terminate_windows_process(*pid))
        .collect::<Vec<_>>();
    Json(serde_json::json!({ "success": true, "killed": killed })).into_response()
}

pub fn ports_routes() -> axum::Router {
    axum::Router::new()
        .route("/ports", axum::routing::get(list_ports))
        .route("/ports/kill", axum::routing::post(kill_port))
}
