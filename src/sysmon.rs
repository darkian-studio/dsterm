use axum::{response::IntoResponse, Json};
use serde::Serialize;
use std::fs;
use sysinfo::{Disks, System};

#[derive(Debug, Serialize)]
struct CpuInfo {
    brand: String,
    cores: usize,
    usage_percent: f32,
}

#[derive(Debug, Serialize)]
struct MemoryInfo {
    total: u64,
    used: u64,
    available: u64,
}

#[derive(Debug, Serialize)]
struct DiskInfo {
    name: String,
    mount_point: String,
    total: u64,
    available: u64,
}

#[derive(Debug, Serialize)]
struct BatteryInfo {
    percent: u8,
    status: String,
}

#[derive(Debug, Serialize)]
struct SysmonResponse {
    cpu: CpuInfo,
    memory: MemoryInfo,
    uptime_secs: u64,
    disks: Vec<DiskInfo>,
    battery: Option<BatteryInfo>,
}

fn read_battery() -> Option<BatteryInfo> {
    let entries = fs::read_dir("/sys/class/power_supply").ok()?;
    for entry in entries.flatten() {
        let path = entry.path();
        let is_battery = fs::read_to_string(path.join("type"))
            .map(|value| value.trim() == "Battery")
            .unwrap_or(false);
        if !is_battery {
            continue;
        }
        let percent = match fs::read_to_string(path.join("capacity")) {
            Ok(value) => match value.trim().parse::<u8>() {
                Ok(percent) => percent,
                Err(_) => continue,
            },
            Err(_) => continue,
        };
        let status = fs::read_to_string(path.join("status"))
            .map(|value| value.trim().to_string())
            .unwrap_or_else(|_| "Unknown".to_string());
        return Some(BatteryInfo { percent, status });
    }
    None
}

pub fn snapshot_json() -> serde_json::Value {
    let mut system = System::new_all();
    system.refresh_all();
    let cpus = system.cpus();
    let usage = if cpus.is_empty() {
        0.0
    } else {
        cpus.iter().map(|cpu| cpu.cpu_usage()).sum::<f32>() / cpus.len() as f32
    };
    let disks = Disks::new_with_refreshed_list()
        .iter()
        .map(|disk| DiskInfo {
            name: disk.name().to_string_lossy().into_owned(),
            mount_point: disk.mount_point().to_string_lossy().into_owned(),
            total: disk.total_space(),
            available: disk.available_space(),
        })
        .collect::<Vec<_>>();
    serde_json::to_value(SysmonResponse {
        cpu: CpuInfo {
            brand: cpus
                .first()
                .map(|cpu| cpu.brand().to_string())
                .unwrap_or_default(),
            cores: cpus.len(),
            usage_percent: usage,
        },
        memory: MemoryInfo {
            total: system.total_memory(),
            used: system.used_memory(),
            available: system.available_memory(),
        },
        uptime_secs: System::uptime(),
        disks,
        battery: read_battery(),
    })
    .unwrap_or_else(|_| serde_json::json!({}))
}

pub async fn get_sysmon() -> impl IntoResponse {
    Json(snapshot_json())
}

pub fn sysmon_routes() -> axum::Router {
    axum::Router::new().route("/sysmon", axum::routing::get(get_sysmon))
}
