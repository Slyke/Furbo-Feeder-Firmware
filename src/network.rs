//! Root-owned Wi-Fi management with explicit, reboot-safe confirmation.
use anyhow::{ensure, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    fs::{self, OpenOptions},
    os::fd::AsRawFd,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};
use tokio::process::{Child, Command};
const ROOT: &str = "/usr/local/furbo-local/network";
const RUN: &str = "/run/furbo-network";
const CONFIRM_SECONDS: f64 = 300.0;
#[derive(Deserialize, Serialize)]
pub struct Settings {
    pub recovery_ssid: String,
    pub recovery_password: String,
    pub reset_pressed_value: String,
    #[serde(default = "default_ntp_servers")]
    pub ntp_servers: Vec<String>,
}
#[derive(Clone, Deserialize, Serialize)]
struct Trial {
    id: String,
    boot_id: String,
    deadline_uptime: f64,
}
fn default_ntp_servers() -> Vec<String> {
    vec!["time.cloudflare.com".into(), "time.google.com".into()]
}
fn valid_ntp_servers(servers: &[String]) -> bool {
    servers.len() <= 4
        && servers.iter().all(|server| {
            !server.is_empty()
                && server.len() <= 253
                && server
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b".-:[]".contains(&b))
        })
}
fn root() -> PathBuf {
    PathBuf::from(ROOT)
}
fn runtime() -> PathBuf {
    PathBuf::from(RUN)
}
fn boot_id() -> Result<String> {
    Ok(fs::read_to_string("/proc/sys/kernel/random/boot_id")?
        .trim()
        .to_owned())
}
fn uptime() -> Result<f64> {
    Ok(fs::read_to_string("/proc/uptime")?
        .split_whitespace()
        .next()
        .context("uptime")?
        .parse()?)
}
fn trial() -> Result<Option<Trial>> {
    let p = root().join("pending.json");
    if !p.exists() {
        return Ok(None);
    }
    ensure!(
        fs::metadata(&p)?.len() < 4096,
        "invalid Wi-Fi trial metadata"
    );
    Ok(Some(serde_json::from_slice(&fs::read(p)?)?))
}
fn valid_trial(t: &Trial, boot: &str, time: f64) -> bool {
    t.boot_id == boot && time < t.deadline_uptime
}
fn discard_trial() -> Result<()> {
    for name in ["pending.json", "station.pending"] {
        match fs::remove_file(root().join(name)) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
    }
    std::fs::File::open(ROOT)?.sync_all()?;
    Ok(())
}
pub fn init() -> Result<()> {
    ensure!(unsafe { libc::geteuid() } == 0, "network init needs root");
    crate::store::private_dir(Path::new(ROOT))?;
    let path = root().join("recovery.json");
    ensure!(!path.exists(), "recovery credentials already initialized");
    let s = Settings {
        recovery_ssid: "furbo2".into(),
        recovery_password: "furbo2setup".into(),
        reset_pressed_value: "1".into(),
        ntp_servers: default_ntp_servers(),
    };
    crate::store::atomic_write(&path, &serde_json::to_vec(&s)?)?;
    println!(
        "Recovery Wi-Fi: {}\nRecovery password: {}\nSetup URL: https://192.168.42.1:8443",
        s.recovery_ssid, s.recovery_password
    );
    Ok(())
}
pub fn status() -> Result<Value> {
    let p = runtime().join("status.json");
    if p.exists() {
        let _ = crate::store::atomic_write(&runtime().join("activity"), b"1");
        Ok(serde_json::from_slice(&fs::read(p)?)?)
    } else {
        Ok(json!({"mode":"not_running"}))
    }
}
pub fn configure(ssid: &str, passphrase: &str) -> Result<Value> {
    ensure!(
        runtime().join("status.json").exists(),
        "network manager is not running"
    );
    ensure!(
        trial()?.is_none(),
        "a Wi-Fi trial is active; confirm or roll it back first"
    );
    let body = station_profile(ssid, passphrase)?;
    let t = Trial {
        id: crate::auth::random(18),
        boot_id: boot_id()?,
        deadline_uptime: uptime()? + CONFIRM_SECONDS,
    };
    crate::store::atomic_write(&root().join("station.pending"), body.as_bytes())?;
    crate::store::atomic_write(&root().join("pending.json"), &serde_json::to_vec(&t)?)?;
    crate::store::atomic_write(&runtime().join("apply"), t.id.as_bytes())?;
    Ok(
        json!({"transaction_id":t.id,"requires_confirmation":true,"expires_in":300,"reboot_rolls_back":true}),
    )
}
pub fn confirm(id: &str) -> Result<Value> {
    let t = trial()?.context("no Wi-Fi trial to confirm")?;
    ensure!(
        id == t.id && valid_trial(&t, &boot_id()?, uptime()?),
        "expired or incorrect Wi-Fi transaction"
    );
    let s: Value = serde_json::from_slice(&fs::read(runtime().join("status.json"))?)?;
    ensure!(
        s["mode"] == "trial" && s["connected"] == true && s["transaction_id"] == id,
        "trial Wi-Fi is not connected"
    );
    fs::rename(root().join("station.pending"), root().join("station.conf"))?;
    std::fs::File::open(ROOT)?.sync_all()?;
    fs::remove_file(root().join("pending.json"))?;
    std::fs::File::open(ROOT)?.sync_all()?;
    Ok(json!({"confirmed":true,"transaction_id":id}))
}
pub fn rollback() -> Result<Value> {
    discard_trial()?;
    crate::store::atomic_write(&runtime().join("rollback"), b"1")?;
    Ok(json!({"rollback_queued":true}))
}
pub fn recover() -> Result<()> {
    crate::store::atomic_write(&runtime().join("recover"), b"1")
}
pub fn station_profile(ssid: &str, passphrase: &str) -> Result<String> {
    ensure!(
        !ssid.is_empty() && ssid.len() <= 32,
        "SSID must be 1..32 UTF-8 bytes"
    );
    let mut psk = [0u8; 32];
    if passphrase.len() == 64 && passphrase.bytes().all(|b| b.is_ascii_hexdigit()) {
        for (i, byte) in psk.iter_mut().enumerate() {
            *byte = u8::from_str_radix(&passphrase[i * 2..i * 2 + 2], 16)?;
        }
    } else {
        ensure!(
            (8..=63).contains(&passphrase.len()),
            "WPA2 passphrase must be 8..63 bytes or a 64-digit hexadecimal PSK"
        );
        pbkdf2::pbkdf2_hmac::<sha1::Sha1>(passphrase.as_bytes(), ssid.as_bytes(), 4096, &mut psk);
    }
    let hex = |b: &[u8]| b.iter().map(|v| format!("{v:02x}")).collect::<String>();
    Ok(format!("ctrl_interface=/var/run/wpa_supplicant\nupdate_config=0\nnetwork={{\n ssid={}\n psk={}\n key_mgmt=WPA-PSK\n proto=RSN\n pairwise=CCMP\n scan_ssid=1\n}}\n",hex(ssid.as_bytes()),hex(&psk)))
}
async fn output(program: &str, args: &[&str]) -> Result<String> {
    let out = tokio::time::timeout(
        Duration::from_secs(8),
        Command::new(program).args(args).kill_on_drop(true).output(),
    )
    .await??;
    ensure!(
        out.status.success() && out.stdout.len() < 32768,
        "network utility failed"
    );
    Ok(String::from_utf8_lossy(&out.stdout).to_string())
}
async fn utility(program: &str, args: &[&str]) -> Result<()> {
    let status = tokio::time::timeout(
        Duration::from_secs(8),
        Command::new(program)
            .args(args)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(true)
            .status(),
    )
    .await??;
    ensure!(status.success(), "network utility failed");
    Ok(())
}
fn spawn(program: &str, args: &[&str]) -> Result<Child> {
    Ok(Command::new(program)
        .args(args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true)
        .spawn()?)
}
async fn stop_interface(children: &mut Vec<Child>) {
    for child in children.iter_mut() {
        let _ = child.start_kill();
        let _ = child.wait().await;
    }
    children.clear();
    if let Ok(entries) = fs::read_dir("/proc") {
        for e in entries.flatten() {
            let Some(pid) = e.file_name().to_str().and_then(|s| s.parse::<i32>().ok()) else {
                continue;
            };
            let Ok(bytes) = fs::read(e.path().join("cmdline")) else {
                continue;
            };
            let args: Vec<_> = bytes
                .split(|b| *b == 0)
                .filter_map(|b| std::str::from_utf8(b).ok())
                .collect();
            let name = args
                .first()
                .and_then(|s| Path::new(s).file_name())
                .and_then(|s| s.to_str())
                .unwrap_or("");
            if ["wpa_supplicant", "udhcpc", "udhcpd"].contains(&name)
                && args.iter().any(|a| {
                    *a == "wlan0" || *a == "-iwlan0" || *a == "/run/furbo-network/dhcp.conf"
                })
            {
                unsafe {
                    libc::kill(pid, libc::SIGTERM);
                }
            }
        }
    }
    tokio::time::sleep(Duration::from_millis(500)).await;
}
async fn radio(children: &mut Vec<Child>, ap: bool) -> Result<()> {
    stop_interface(children).await;
    let _ = utility("/sbin/ifconfig", &["wlan0", "down"]).await;
    if Path::new("/sys/module/cywdhd").exists() {
        utility("/sbin/modprobe", &["-r", "cywdhd"]).await?;
    }
    utility(
        "/sbin/modprobe",
        &[
            "cywdhd",
            "iface_name=wlan0",
            "dhd_msg_level=0x00",
            if ap { "op_mode=2" } else { "op_mode=1" },
        ],
    )
    .await?;
    for _ in 0..30 {
        if Path::new("/sys/class/net/wlan0").exists() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    utility("/sbin/ifconfig", &["wlan0", "up"]).await?;
    Ok(())
}
async fn station(children: &mut Vec<Child>, candidate: bool) -> Result<()> {
    let p = root().join(if candidate {
        "station.pending"
    } else {
        "station.conf"
    });
    ensure!(p.exists(), "no saved Wi-Fi profile");
    radio(children, false).await?;
    children.push(spawn(
        "/usr/sbin/wpa_supplicant",
        &[
            "-i",
            "wlan0",
            "-D",
            "wext",
            "-c",
            p.to_str().context("station path")?,
        ],
    )?);
    Ok(())
}
async fn access_point(children: &mut Vec<Child>, settings: &Settings) -> Result<()> {
    radio(children, true).await?;
    let p = station_profile(&settings.recovery_ssid, &settings.recovery_password)?
        .replace(" scan_ssid=1", " mode=2\n frequency=2437");
    crate::store::atomic_write(&root().join("ap.conf"), p.as_bytes())?;
    children.push(spawn(
        "/usr/sbin/wpa_supplicant",
        &[
            "-i",
            "wlan0",
            "-D",
            "nl80211",
            "-c",
            "/usr/local/furbo-local/network/ap.conf",
        ],
    )?);
    tokio::time::sleep(Duration::from_secs(2)).await;
    ensure!(
        children
            .last_mut()
            .context("AP process")?
            .try_wait()?
            .is_none(),
        "AP supplicant exited"
    );
    let info = output("/usr/sbin/iw", &["dev", "wlan0", "info"]).await?;
    ensure!(
        info.lines().any(|l| l.trim() == "type AP"),
        "radio did not enter AP mode"
    );
    utility(
        "/sbin/ifconfig",
        &["wlan0", "192.168.42.1", "netmask", "255.255.255.0", "up"],
    )
    .await?;
    let dhcp="start 192.168.42.20\nend 192.168.42.50\ninterface wlan0\noption subnet 255.255.255.0\noption router 192.168.42.1\noption dns 192.168.42.1\noption lease 600\nlease_file /run/furbo-network/leases\npidfile /run/furbo-network/dhcp.pid\n";
    crate::store::atomic_write(&runtime().join("dhcp.conf"), dhcp.as_bytes())?;
    children.push(spawn(
        "/bin/busybox",
        &["udhcpd", "-f", "/run/furbo-network/dhcp.conf"],
    )?);
    Ok(())
}
pub fn sync_auth_marker() -> Result<()> {
    use std::os::unix::{ffi::OsStrExt, fs::PermissionsExt};
    let marker = root().join("password-reset-required");
    let generation = fs::read_to_string(marker).ok();
    let value = json!({"setup_required":generation.is_some(),"generation":generation});
    let p = Path::new("/run/furbo-local/auth-reset.json");
    crate::store::atomic_write(p, &serde_json::to_vec(&value)?)?;
    let c = std::ffi::CString::new(p.as_os_str().as_bytes())?;
    ensure!(
        unsafe { libc::chown(c.as_ptr(), 0, 33) } == 0,
        "auth state ownership failed"
    );
    fs::set_permissions(p, fs::Permissions::from_mode(0o640))?;
    Ok(())
}
pub fn auth_state() -> Result<Value> {
    let generation = fs::read_to_string(root().join("password-reset-required")).ok();
    let p = runtime().join("status.json");
    let fresh = fs::metadata(&p)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.elapsed().ok())
        .is_some_and(|d| d < Duration::from_secs(5));
    let recovery_active = fresh
        && fs::read(&p)
            .ok()
            .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
            .is_some_and(|v| v["mode"] == "recovery");
    Ok(
        json!({"setup_required":generation.is_some(),"generation":generation,"recovery_active":recovery_active}),
    )
}
pub fn complete_password_setup(generation: Option<String>) -> Result<()> {
    let marker = root().join("password-reset-required");
    let current = fs::read_to_string(&marker).ok();
    ensure!(
        current == generation,
        "another reset happened; repeat password setup"
    );
    if current.is_some() {
        fs::remove_file(marker)?;
        std::fs::File::open(ROOT)?.sync_all()?;
    }
    sync_auth_marker()
}
pub fn factory_reset() -> Result<()> {
    ensure!(unsafe { libc::geteuid() } == 0, "reset requires root");
    discard_trial()?;
    for path in [
        root().join("station.conf"),
        PathBuf::from("/usr/local/furbo-ssh/etc/wpa_supplicant.conf"),
    ] {
        match fs::remove_file(path) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
    }
    let ntp_servers = fs::read(root().join("recovery.json"))
        .ok()
        .and_then(|bytes| serde_json::from_slice::<Settings>(&bytes).ok())
        .map(|settings| settings.ntp_servers)
        .unwrap_or_else(default_ntp_servers);
    let settings = Settings {
        recovery_ssid: "furbo2".into(),
        recovery_password: "furbo2setup".into(),
        reset_pressed_value: "1".into(),
        ntp_servers,
    };
    crate::store::atomic_write(
        &root().join("recovery.json"),
        &serde_json::to_vec(&settings)?,
    )?;
    crate::store::atomic_write(
        &root().join("password-reset-required"),
        crate::auth::random(24).as_bytes(),
    )?;
    sync_auth_marker()?;
    let path = Path::new("/usr/local/furbo-local/etc/config.json");
    let mut cfg = crate::config::Config::load(path)?;
    crate::recovery::web_defaults(&mut cfg)?;
    crate::recovery::write_owned(path, &serde_json::to_vec_pretty(&cfg)?)?;
    std::process::Command::new("/usr/bin/systemctl")
        .args(["restart", "furbo-local.service"])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()?;
    recover()
}
pub async fn serve() -> Result<()> {
    ensure!(
        unsafe { libc::geteuid() } == 0,
        "network manager needs root"
    );
    crate::store::private_dir(Path::new(RUN))?;
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(runtime().join("lock"))?;
    ensure!(
        unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0,
        "network manager already running"
    );
    let mut settings: Settings = serde_json::from_slice(&fs::read(root().join("recovery.json"))?)?;
    ensure!(
        ["0", "1"].contains(&settings.reset_pressed_value.as_str()),
        "invalid reset polarity"
    );
    ensure!(
        valid_ntp_servers(&settings.ntp_servers),
        "invalid NTP servers"
    );
    let mut time_client: Option<Child> = None;
    let mut time_attempt = Instant::now() - Duration::from_secs(30);
    let boot = boot_id()?;
    if trial()?.is_some_and(|t| !valid_trial(&t, &boot, uptime().unwrap_or(f64::MAX))) {
        discard_trial()?;
    }
    let mut children = vec![];
    let mut portal: Option<crate::portal::Portal> = None;
    let mut mode = "station";
    let mut since = Instant::now();
    let mut candidate = false;
    let mut dhcp_started = false;
    let mut initial = true;
    let mut pressed: Option<Instant> = None;
    let mut armed = false;
    let mut force_recovery = !root().join("station.conf").exists();
    let mut last_error: Option<&str> = None;
    loop {
        tokio::time::sleep(Duration::from_millis(250)).await;
        if let Ok(raw) = fs::read_to_string("/sys/class/gpio/gpio53/value") {
            if raw.trim() != settings.reset_pressed_value {
                armed = true;
                pressed = None;
            } else if armed {
                let start = pressed.get_or_insert_with(Instant::now);
                if start.elapsed() >= Duration::from_secs(10) {
                    factory_reset()?;
                    settings = serde_json::from_slice(&fs::read(root().join("recovery.json"))?)?;
                    force_recovery = true;
                    armed = false;
                    pressed = None;
                }
            }
        }
        let current = trial()?;
        if current
            .as_ref()
            .is_some_and(|t| !valid_trial(t, &boot, uptime().unwrap_or(f64::MAX)))
        {
            discard_trial()?;
            let _ = fs::remove_file(runtime().join("apply"));
            last_error = Some("confirmation_timeout");
            candidate = false;
            portal.take();
            if station(&mut children, false).await.is_err() {
                force_recovery = true;
            }
            mode = "station";
            since = Instant::now();
            dhcp_started = false;
            initial = false;
        } else if candidate && current.is_none() {
            // The helper durably committed an explicit confirmation.
            candidate = false;
            mode = "station";
        }
        if runtime().join("rollback").exists() {
            let _ = fs::remove_file(runtime().join("rollback"));
            discard_trial()?;
            candidate = false;
            mode = "station";
            since = Instant::now();
            dhcp_started = false;
            initial = false;
            portal.take();
            if station(&mut children, false).await.is_err() {
                force_recovery = true;
            }
        }
        if runtime().join("recover").exists() {
            let _ = fs::remove_file(runtime().join("recover"));
            force_recovery = true;
        }
        if runtime().join("apply").exists() {
            // Give the API/MQTT acknowledgment time to leave the old connection.
            let ready = fs::metadata(runtime().join("apply"))
                .and_then(|m| m.modified())
                .ok()
                .and_then(|t| t.elapsed().ok())
                .is_some_and(|d| d >= Duration::from_secs(2));
            if ready {
                let _ = fs::remove_file(runtime().join("apply"));
                if trial()?.is_some() {
                    candidate = true;
                    mode = "trial";
                    since = Instant::now();
                    dhcp_started = false;
                    initial = false;
                    portal.take();
                    if station(&mut children, true).await.is_err() {
                        last_error = Some("trial_start_failed");
                        discard_trial()?;
                        candidate = false;
                        force_recovery = true;
                    }
                }
            }
        }
        if force_recovery {
            force_recovery = false;
            discard_trial()?;
            candidate = false;
            portal.take();
            if access_point(&mut children, &settings).await.is_ok() {
                portal = Some(crate::portal::start().await?);
                mode = "recovery";
            } else {
                mode = "ap_failed";
                last_error = Some("ap_start_failed");
            }
            since = Instant::now();
            dhcp_started = false;
            initial = false;
        }
        if initial && !force_recovery {
            initial = false;
            candidate = trial()?.is_some();
            mode = if candidate { "trial" } else { "station" };
            since = Instant::now();
            dhcp_started = false;
            portal.take();
            if station(&mut children, candidate).await.is_err() {
                last_error = Some("station_start_failed");
                force_recovery = true;
            }
        }
        if mode == "station" || mode == "trial" {
            let status = output(
                "/usr/sbin/wpa_cli",
                &["-p", "/var/run/wpa_supplicant", "-i", "wlan0", "status"],
            )
            .await
            .unwrap_or_default();
            let associated = status.lines().any(|s| s == "wpa_state=COMPLETED")
                && status.lines().any(|s| s == "mode=station");
            let address = status
                .lines()
                .find_map(|s| s.strip_prefix("ip_address="))
                .filter(|s| *s != "192.168.42.1")
                .map(str::to_owned);
            if associated && address.is_some() {
                // BusyBox runs a client only (no -l server listener). Keep it
                // separate from radio children so it can maintain clock drift
                // across a station reconnect.
                if time_client
                    .as_mut()
                    .is_some_and(|child| child.try_wait().ok().flatten().is_some())
                {
                    time_client.take();
                }
                if time_client.is_none()
                    && !settings.ntp_servers.is_empty()
                    && time_attempt.elapsed() >= Duration::from_secs(30)
                {
                    time_attempt = Instant::now();
                    let mut args = vec!["ntpd", "-n"];
                    for server in &settings.ntp_servers {
                        args.extend(["-p", server.as_str()]);
                    }
                    match spawn("/bin/busybox", &args) {
                        Ok(child) => time_client = Some(child),
                        Err(_) => crate::log_error!("could not start NTP client; retrying"),
                    }
                }
                since = Instant::now();
                initial = false;
                if !candidate {
                    last_error = None;
                }
            } else {
                if associated && !dhcp_started {
                    if let Ok(c) = spawn(
                        "/bin/busybox",
                        &[
                            "udhcpc",
                            "-f",
                            "-i",
                            "wlan0",
                            "-p",
                            "/run/furbo-network/client.pid",
                            "-x",
                            "hostname:furbo-local",
                        ],
                    ) {
                        children.push(c);
                        dhcp_started = true;
                    }
                }
                if initial && since.elapsed() > Duration::from_secs(5) {
                    initial = false;
                    since = Instant::now();
                    candidate = trial()?.is_some();
                    mode = if candidate { "trial" } else { "station" };
                    dhcp_started = false;
                    if station(&mut children, candidate).await.is_err() {
                        force_recovery = true;
                        last_error = Some("station_start_failed");
                    }
                }
                if since.elapsed() > Duration::from_secs(90) {
                    if candidate {
                        discard_trial()?;
                        candidate = false;
                        mode = "station";
                        since = Instant::now();
                        dhcp_started = false;
                        last_error = Some("trial_connection_failed");
                        portal.take();
                        if station(&mut children, false).await.is_err() {
                            force_recovery = true;
                        }
                    } else {
                        force_recovery = true;
                    }
                }
            }
            let t = trial()?;
            let value = json!({"mode":mode,"connected":associated&&address.is_some(),"address":address,"pending":candidate,"transaction_id":t.as_ref().map(|t|&t.id),"confirmation_seconds_remaining":t.map(|t|(t.deadline_uptime-uptime().unwrap_or(t.deadline_uptime)).max(0.0)as u64),"last_error":last_error});
            crate::store::atomic_write(
                &runtime().join("status.json"),
                &serde_json::to_vec(&value)?,
            )?;
        } else {
            let inactive = fs::metadata(runtime().join("activity"))
                .and_then(|m| m.modified())
                .ok()
                .and_then(|t| t.elapsed().ok())
                .unwrap_or_else(|| since.elapsed());
            if root().join("station.conf").exists()
                && since.elapsed() > Duration::from_secs(600)
                && inactive > Duration::from_secs(600)
                || mode == "ap_failed" && since.elapsed() > Duration::from_secs(15)
            {
                mode = "station";
                since = Instant::now();
                dhcp_started = false;
                portal.take();
                if station(&mut children, false).await.is_err() {
                    force_recovery = true;
                }
            }
            crate::store::atomic_write(
                &runtime().join("status.json"),
                &serde_json::to_vec(
                    &json!({"mode":mode,"ssid":settings.recovery_ssid,"address":"192.168.42.1","pending":false,"last_error":last_error}),
                )?,
            )?;
        }
        tokio::time::sleep(Duration::from_millis(750)).await;
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn time_server_defaults_and_bounds() {
        let settings: Settings = serde_json::from_str(
            r#"{"recovery_ssid":"furbo2","recovery_password":"furbo2setup","reset_pressed_value":"1"}"#
        ).unwrap();
        assert_eq!(settings.ntp_servers, default_ntp_servers());
        assert!(valid_ntp_servers(&settings.ntp_servers));
        assert!(valid_ntp_servers(&[]));
        assert!(valid_ntp_servers(&[
            "192.0.2.1".into(),
            "2001:db8::1".into()
        ]));
        assert!(!valid_ntp_servers(&["server --extra-option".into()]));
        assert!(!valid_ntp_servers(&vec!["example.invalid".into(); 5]));
    }
    #[test]
    fn wifi_escaping() {
        let s = station_profile("name\"\nnetwork={", "password-with-quotes\"").unwrap();
        assert!(!s.contains("name"));
        assert_eq!(s.matches("network={").count(), 1);
        assert!(!s.contains("password-with"));
        assert!(station_profile("", "short").is_err());
    }
    #[test]
    fn wpa_vector() {
        assert!(station_profile("IEEE", "password")
            .unwrap()
            .contains("f42c6fc52df0ebef9ebb4b90b38a5f902e83fe1b135a70e23aed762e9710a12e"));
    }
    #[test]
    fn confirmation_deadline_and_reboot() {
        let t = Trial {
            id: "a".into(),
            boot_id: "boot-one".into(),
            deadline_uptime: 400.0,
        };
        assert!(valid_trial(&t, "boot-one", 399.0));
        assert!(!valid_trial(&t, "boot-one", 400.0));
        assert!(!valid_trial(&t, "boot-two", 1.0));
    }
}
