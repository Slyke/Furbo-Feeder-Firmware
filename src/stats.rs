use crate::config::Config;
use serde_json::{json, Value};
use std::{collections::BTreeMap, path::Path};
fn text(path: &str) -> Option<String> {
    std::fs::read_to_string(path)
        .ok()
        .map(|s| s.trim_matches(['\0', '\n', '\r', ' ']).to_owned())
        .filter(|s| !s.is_empty())
}
fn number(path: &str) -> Option<u64> {
    text(path)?.parse().ok()
}
fn identity() -> BTreeMap<String, String> {
    text("/proc/cpuinfo")
        .unwrap_or_default()
        .lines()
        .filter_map(|l| {
            l.split_once(':')
                .map(|(k, v)| (k.trim().to_owned(), v.trim().to_owned()))
        })
        .collect()
}
fn memory() -> Value {
    let m: BTreeMap<String, u64> = text("/proc/meminfo")
        .unwrap_or_default()
        .lines()
        .filter_map(|l| {
            let (k, v) = l.split_once(':')?;
            Some((
                k.into(),
                v.split_whitespace()
                    .next()?
                    .parse::<u64>()
                    .ok()?
                    .saturating_mul(1024),
            ))
        })
        .collect();
    let get = |s: &str| *m.get(s).unwrap_or(&0);
    let total = get("MemTotal");
    let estimate = (get("MemFree") + get("Buffers") + get("Cached") + get("SReclaimable"))
        .saturating_sub(get("Shmem"))
        .min(total);
    json!({"total_bytes":total,"free_bytes":get("MemFree"),"available_bytes":m.get("MemAvailable").copied().unwrap_or(estimate),"available_is_estimate":!m.contains_key("MemAvailable"),"cached_bytes":get("Cached"),"shared_bytes":get("Shmem"),"swap_total_bytes":get("SwapTotal"),"swap_free_bytes":get("SwapFree")})
}
// libc field widths differ between the ARM32 target and the 64-bit build host.
#[allow(clippy::unnecessary_cast)]
pub fn filesystem(path: &Path) -> Value {
    use std::os::unix::ffi::OsStrExt;
    let result = (|| {
        let name = std::ffi::CString::new(path.as_os_str().as_bytes()).ok()?;
        let mut data = std::mem::MaybeUninit::<libc::statvfs>::uninit();
        if unsafe { libc::statvfs(name.as_ptr(), data.as_mut_ptr()) } != 0 {
            return None;
        }
        let v = unsafe { data.assume_init() };
        let block = if v.f_frsize == 0 {
            v.f_bsize
        } else {
            v.f_frsize
        } as u64;
        Some(
            json!({"path":path.to_string_lossy(),"total_bytes":(v.f_blocks as u64)*block,"free_bytes":(v.f_bfree as u64)*block,"available_bytes":(v.f_bavail as u64)*block,"used_bytes":((v.f_blocks-v.f_bfree)as u64)*block,"read_only":v.f_flag&libc::ST_RDONLY as libc::c_ulong!=0}),
        )
    })();
    result.unwrap_or_else(|| json!({"path":path.to_string_lossy(),"available":false}))
}
pub fn collect(cfg: &Config) -> Value {
    let cpu = identity();
    let soc_serial = cpu.get("Serial").cloned();
    let nonzero = |v: &String| v.bytes().any(|b| b != b'0' && b != b'-' && b != b' ');
    let serial = cfg
        .serial_number
        .clone()
        .or_else(|| text("/dev/adc/SN.txt").filter(nonzero))
        .or_else(|| text("/proc/device-tree/serial-number").filter(nonzero))
        .or_else(|| soc_serial.clone().filter(nonzero));
    let processes = std::fs::read_to_string("/proc/self/status").unwrap_or_default();
    let proc_kb = |key: &str| {
        processes
            .lines()
            .find(|l| l.starts_with(key))
            .and_then(|l| l.split_whitespace().nth(1)?.parse::<u64>().ok())
            .map(|n| n * 1024)
    };
    let uptime =
        text("/proc/uptime").and_then(|s| s.split_whitespace().next()?.parse::<f64>().ok());
    let mut network = serde_json::Map::new();
    for field in [
        "rx_bytes",
        "tx_bytes",
        "rx_packets",
        "tx_packets",
        "rx_errors",
        "tx_errors",
        "rx_dropped",
        "tx_dropped",
    ] {
        network.insert(
            field.into(),
            json!(number(&format!("/sys/class/net/wlan0/statistics/{field}"))),
        );
    }
    network.insert("interface".into(), json!("wlan0"));
    network.insert("mac".into(), json!(text("/sys/class/net/wlan0/address")));
    network.insert(
        "operstate".into(),
        json!(text("/sys/class/net/wlan0/operstate")),
    );
    network.insert(
        "wireless".into(),
        json!(text("/proc/net/wireless").and_then(|s| s
            .lines()
            .find(|l| l.contains("wlan0:"))
            .map(|l| l.trim().to_owned()))),
    );
    json!({"device":{"id":cfg.device_id,"model":"Furbo2","platform":cpu.get("Hardware"),"serial_number":serial,"device_uuid":text("/dev/adc/furbo2_uuid.cfg"),"soc_serial":soc_serial,"mac_address":text("/sys/class/net/wlan0/address")},"software":{"version":env!("CARGO_PKG_VERSION"),"tzdb_version":crate::timezone::VERSION.trim(),"kernel":text("/proc/sys/kernel/osrelease"),"architecture":std::env::consts::ARCH,"clock_valid":crate::model::clock_valid(),"timestamp":crate::model::now()},"cpu":{"model":cpu.get("model name"),"cores":std::thread::available_parallelism().map(|n|n.get()).unwrap_or(1),"frequency_khz":number("/sys/devices/system/cpu/cpu0/cpufreq/scaling_cur_freq"),"load_average":text("/proc/loadavg").map(|s|s.split_whitespace().take(3).filter_map(|v|v.parse::<f64>().ok()).collect::<Vec<_>>())},"uptime_seconds":uptime,"memory":memory(),"process":{"rss_bytes":proc_kb("VmRSS:"),"virtual_bytes":proc_kb("VmSize:")},"storage":[filesystem(Path::new("/")),filesystem(Path::new("/dev/adc")),filesystem(&cfg.data_dir),filesystem(&crate::media_dir(cfg))],"media_quota":{"max_bytes":cfg.media.max_bytes,"max_files":cfg.media.max_files},"network":network})
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn statvfs_measures_real_capacity() {
        let v = filesystem(Path::new("/"));
        assert!(v["total_bytes"].as_u64().unwrap() > 0);
        assert!(v["free_bytes"].as_u64().unwrap() <= v["total_bytes"].as_u64().unwrap());
    }
}
