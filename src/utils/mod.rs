use std::net::Ipv4Addr;

#[cfg(not(windows))]
use pnet::datalink;

#[cfg(not(windows))]
pub fn get_ip_address() -> Option<Ipv4Addr> {
    for iface in datalink::interfaces() {
        for ip in iface.ips {
            if let pnet::ipnetwork::IpNetwork::V4(network) = ip {
                if !network.ip().is_loopback() {
                    return Some(network.ip());
                }
            }
        }
    }
    None
}

#[cfg(windows)]
fn windows_ipv4_address(bytes: &[i8; 16]) -> Option<Ipv4Addr> {
    let len = bytes.iter().position(|byte| *byte == 0)?;
    let value = bytes[..len]
        .iter()
        .map(|byte| *byte as u8)
        .collect::<Vec<_>>();
    std::str::from_utf8(&value).ok()?.parse::<Ipv4Addr>().ok()
}

#[cfg(windows)]
pub fn get_ip_address() -> Option<Ipv4Addr> {
    use std::{mem, mem::MaybeUninit, ptr};
    use windows_sys::Win32::{
        Foundation::ERROR_BUFFER_OVERFLOW,
        NetworkManagement::IpHelper::{
            GetAdaptersInfo, IF_TYPE_SOFTWARE_LOOPBACK, IP_ADAPTER_INFO, IP_ADDR_STRING,
        },
    };

    let mut byte_len = 0u32;
    let first_status = unsafe { GetAdaptersInfo(ptr::null_mut(), &mut byte_len) };
    if first_status != ERROR_BUFFER_OVERFLOW || byte_len == 0 {
        return None;
    }

    // The adapter list can grow between calls, so retry once with the new size.
    for _ in 0..2 {
        let allocated_bytes = byte_len;
        let adapter_size = mem::size_of::<IP_ADAPTER_INFO>();
        let entries = (allocated_bytes as usize + adapter_size - 1) / adapter_size;
        let mut buffer: Vec<MaybeUninit<IP_ADAPTER_INFO>> = Vec::with_capacity(entries);
        unsafe { buffer.set_len(entries) };

        let status = unsafe {
            GetAdaptersInfo(buffer.as_mut_ptr().cast::<IP_ADAPTER_INFO>(), &mut byte_len)
        };
        if status == 0 {
            let mut adapter = buffer.as_mut_ptr().cast::<IP_ADAPTER_INFO>();
            while !adapter.is_null() {
                let adapter_ref = unsafe { &*adapter };
                if adapter_ref.Type != IF_TYPE_SOFTWARE_LOOPBACK {
                    let mut address: *const IP_ADDR_STRING = &adapter_ref.IpAddressList;
                    while !address.is_null() {
                        let address_ref = unsafe { &*address };
                        if let Some(ip) = windows_ipv4_address(&address_ref.IpAddress.String) {
                            if !ip.is_loopback() && !ip.is_unspecified() {
                                return Some(ip);
                            }
                        }
                        address = address_ref.Next;
                    }
                }
                adapter = adapter_ref.Next;
            }
            return None;
        }
        if status != ERROR_BUFFER_OVERFLOW || byte_len <= allocated_bytes {
            return None;
        }
    }

    None
}

pub fn parse_u16(value: &serde_json::Value, field_name: &str) -> Result<u16, String> {
    match value {
        serde_json::Value::Number(n) if n.is_u64() => n
            .as_u64()
            .and_then(|n| u16::try_from(n).ok())
            .ok_or_else(|| format!("{field_name} must be a valid u16.")),
        serde_json::Value::String(s) => s
            .parse::<u16>()
            .map_err(|_| format!("{field_name} must be a valid u16 string.")),
        _ => Err(format!("{field_name} must be a number or a valid string.",)),
    }
}
