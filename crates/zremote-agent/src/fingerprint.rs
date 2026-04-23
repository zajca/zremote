//! Machine fingerprint: a stable, pseudonymous identifier for this host.
//!
//! SHA-256(machine_id || primary_mac) — both inputs are stable across reboots.
//! If neither source is available, falls back to hostname with a warning.

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use sha2::{Digest, Sha256};

/// Compute SHA-256(machine_id || primary_mac) and return it base64url-encoded.
/// Falls back to hashing the hostname if the primary sources are unavailable.
pub fn compute() -> String {
    let mut hasher = Sha256::new();
    let machine_id = read_machine_id();
    let mac = read_primary_mac();

    if machine_id.is_none() && mac.is_none() {
        tracing::warn!(
            "machine_id and primary MAC unavailable — using hostname as fingerprint source"
        );
        let h = hostname::get()
            .map(|h| h.to_string_lossy().into_owned())
            .unwrap_or_else(|_| "unknown".to_string());
        hasher.update(h.as_bytes());
    } else {
        if let Some(ref id) = machine_id {
            hasher.update(id.as_bytes());
        }
        if let Some(ref mac) = mac {
            hasher.update(mac);
        }
    }

    URL_SAFE_NO_PAD.encode(hasher.finalize())
}

/// Read the platform machine ID.
fn read_machine_id() -> Option<String> {
    #[cfg(target_os = "linux")]
    {
        read_machine_id_linux()
    }
    #[cfg(target_os = "macos")]
    {
        read_machine_id_macos()
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        None
    }
}

#[cfg(target_os = "linux")]
fn read_machine_id_linux() -> Option<String> {
    std::fs::read_to_string("/etc/machine-id")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

#[cfg(target_os = "macos")]
fn read_machine_id_macos() -> Option<String> {
    // ioreg -rd1 -c IOPlatformExpertDevice | awk '/IOPlatformUUID/{print $3}'
    let output = std::process::Command::new("ioreg")
        .args(["-rd1", "-c", "IOPlatformExpertDevice"])
        .output()
        .ok()?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    for line in stdout.lines() {
        if line.contains("IOPlatformUUID") {
            // format: "IOPlatformUUID" = "XXXXXXXX-XXXX-..."
            if let Some(uuid) = line.split('"').nth(3) {
                return Some(uuid.to_string());
            }
        }
    }
    None
}

/// Read the MAC address of the primary (lowest-index non-loopback) interface.
fn read_primary_mac() -> Option<Vec<u8>> {
    // Use getifaddrs via nix to enumerate interfaces.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    {
        read_primary_mac_unix()
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        None
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn read_primary_mac_unix() -> Option<Vec<u8>> {
    use std::fs;

    // On Linux, enumerate /sys/class/net and pick the first non-loopback interface.
    #[cfg(target_os = "linux")]
    {
        let mut entries: Vec<_> = fs::read_dir("/sys/class/net")
            .ok()?
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|name| name != "lo")
            .collect();
        entries.sort();
        for iface in entries {
            let mac_path = format!("/sys/class/net/{iface}/address");
            if let Ok(mac_str) = fs::read_to_string(&mac_path) {
                let mac_str = mac_str.trim();
                if mac_str == "00:00:00:00:00:00" {
                    continue;
                }
                let bytes: Vec<u8> = mac_str
                    .split(':')
                    .filter_map(|b| u8::from_str_radix(b, 16).ok())
                    .collect();
                if bytes.len() == 6 {
                    return Some(bytes);
                }
            }
        }
        None
    }
    #[cfg(target_os = "macos")]
    {
        // ifconfig -l to get interface list, then ifconfig <iface> ether
        let output = std::process::Command::new("ifconfig")
            .arg("-l")
            .output()
            .ok()?;
        let ifaces = String::from_utf8_lossy(&output.stdout);
        let mut names: Vec<_> = ifaces
            .split_whitespace()
            .filter(|n| !n.starts_with("lo"))
            .map(str::to_string)
            .collect();
        names.sort();
        for iface in names {
            let out = std::process::Command::new("ifconfig")
                .arg(&iface)
                .output()
                .ok()?;
            let text = String::from_utf8_lossy(&out.stdout);
            for line in text.lines() {
                let trimmed = line.trim();
                if trimmed.starts_with("ether ") {
                    let mac_str = trimmed.trim_start_matches("ether ").trim();
                    let bytes: Vec<u8> = mac_str
                        .split(':')
                        .filter_map(|b| u8::from_str_radix(b, 16).ok())
                        .collect();
                    if bytes.len() == 6 && bytes != [0u8; 6] {
                        return Some(bytes);
                    }
                }
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fingerprint_is_stable_across_calls_within_boot() {
        let a = compute();
        let b = compute();
        assert_eq!(
            a, b,
            "fingerprint must be deterministic within a single boot"
        );
    }

    #[test]
    fn fingerprint_is_non_empty_base64url() {
        let fp = compute();
        assert!(!fp.is_empty());
        // base64url characters only
        assert!(
            fp.chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'),
            "fingerprint must be base64url: {fp}"
        );
    }
}
