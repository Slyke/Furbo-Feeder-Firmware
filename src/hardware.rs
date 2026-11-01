//! Root-only, allowlisted hardware adapter. Never accepts a shell command or a path.
use crate::model::{clock_valid, now, Color, ImageSetting, NightMode};
use anyhow::{ensure, Context, Result};
use base64::Engine;
use serde::{Deserialize, Serialize};
use std::{
    os::unix::fs::{FileTypeExt, MetadataExt, PermissionsExt},
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{UnixListener, UnixStream},
    process::Command,
    sync::Semaphore,
};

async fn native_night(mode: &NightMode) -> Result<serde_json::Value> {
    // Owned factory controller: command 0 followed by its native mode, both LE u32.
    // It acknowledges consumption by closing the stream after writing /etc/IRmode.
    tokio::time::timeout(Duration::from_secs(3), async {
        let mut stream = UnixStream::connect("/tmp/image_control.sock")
            .await
            .context("native camera controller unavailable")?;
        let mut packet = [0u8; 8];
        packet[4..].copy_from_slice(&mode.native_number().to_le_bytes());
        stream.write_all(&packet).await?;
        let mut byte = [0u8; 1];
        ensure!(
            stream.read(&mut byte).await? == 0,
            "invalid image acknowledgment"
        );
        anyhow::Ok(())
    })
    .await??;
    ensure!(
        std::fs::read_to_string("/etc/IRmode")?.trim() == mode.native_number().to_string(),
        "native camera did not accept night mode"
    );
    for _ in 0..50 {
        let enabled = std::fs::read_to_string("/sys/class/gpio/gpio108/value")?.trim() == "1";
        let pwm = std::fs::read_to_string("/sys/class/backlight/0.pwm_bl/brightness")?
            .trim()
            .parse::<u32>()?;
        let applied = match mode {
            NightMode::On => enabled && pwm > 0,
            NightMode::Off => !enabled,
            NightMode::Auto => true,
        };
        if applied {
            return Ok(serde_json::json!({"mode":mode,"ir_enabled":enabled,"pwm":pwm}));
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    anyhow::bail!("night mode requested but physical IR state did not change")
}

pub fn oryx_image_available() -> bool {
    std::fs::read_dir("/proc").ok().is_some_and(|entries| {
        entries.flatten().any(|entry| {
            entry
                .file_name()
                .to_str()
                .is_some_and(|name| name.bytes().all(|b| b.is_ascii_digit()))
                && std::fs::read(entry.path().join("comm"))
                    .ok()
                    .is_some_and(|bytes| bytes == b"img_svc\n" || bytes == b"image_svc\n")
        })
    })
}
const MAX: usize = 2_097_152;
const WORK: &str = "/run/furbo-hw";
#[derive(Clone, Deserialize, Serialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
pub enum Request {
    Overlay { label: String, show_timestamp: bool },
    UpdateConfig { config: Box<crate::config::Config> },
    SetTime { unix_seconds: i64 },
    AuthState,
    PasswordSetupComplete { generation: Option<String> },
    WifiStatus,
    WifiConfirm { transaction_id: String },
    WifiRollback,
    WifiConfigure { ssid: String, passphrase: String },
    WifiRecover,
    Toss,
    Led { color: Color },
    Tunnel { on: bool },
    Night { mode: NightMode },
    Volume { percent: u8 },
    Mute { muted: bool },
    Image { setting: ImageSetting, value: i16 },
    Snapshot,
    Record { seconds: u8 },
    Play { length: usize },
    Lux,
    MotionSample,
    SoundSample,
}
#[derive(Deserialize, Serialize)]
struct Header {
    ok: bool,
    length: usize,
    content_type: String,
    value: serde_json::Value,
    error: Option<String>,
}
pub struct Reply {
    pub bytes: Vec<u8>,
    pub content_type: String,
    pub value: serde_json::Value,
}
pub async fn call(socket: &Path, simulate: bool, request: Request, body: &[u8]) -> Result<Reply> {
    if simulate {
        return simulated(request, body);
    }
    let mut stream = UnixStream::connect(socket)
        .await
        .context("hardware helper unavailable")?;
    tokio::time::timeout(Duration::from_secs(40), async {
        write_json(&mut stream, &request).await?;
        ensure!(body.len() <= MAX, "audio too large");
        stream.write_all(body).await?;
        let h: Header = read_json(&mut stream, 4096).await?;
        ensure!(h.ok, "hardware: {}", h.error.unwrap_or_default());
        ensure!(h.length <= MAX, "invalid hardware response");
        let mut bytes = vec![0; h.length];
        stream.read_exact(&mut bytes).await?;
        Ok(Reply {
            bytes,
            content_type: h.content_type,
            value: h.value,
        })
    })
    .await?
}
async fn write_json<T: Serialize>(s: &mut UnixStream, v: &T) -> Result<()> {
    let b = serde_json::to_vec(v)?;
    s.write_u32(b.len() as u32).await?;
    s.write_all(&b).await?;
    Ok(())
}
async fn read_json<T: serde::de::DeserializeOwned>(s: &mut UnixStream, max: usize) -> Result<T> {
    let n = s.read_u32().await? as usize;
    ensure!(n <= max, "message too large");
    let mut b = vec![0; n];
    s.read_exact(&mut b).await?;
    Ok(serde_json::from_slice(&b)?)
}
pub async fn serve(socket: PathBuf, allowed_uid: u32, allowed_gid: u32) -> Result<()> {
    ensure!(
        unsafe { libc::geteuid() } == 0,
        "hardware helper must run as root"
    );
    let parent = socket.parent().context("socket parent")?;
    std::fs::create_dir_all(parent)?;
    ensure!(
        !std::fs::symlink_metadata(parent)?.file_type().is_symlink(),
        "unsafe socket directory"
    );
    std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o750))?;
    chown(parent, 0, allowed_gid)?;
    crate::store::private_dir(Path::new(WORK))?;
    if socket.exists() {
        let meta = std::fs::symlink_metadata(&socket)?;
        ensure!(
            meta.file_type().is_socket() && meta.uid() == 0,
            "refusing to replace non-root socket"
        );
        ensure!(
            UnixStream::connect(&socket).await.is_err(),
            "hardware helper already running"
        );
        std::fs::remove_file(&socket)?;
    }
    crate::network::sync_auth_marker()?;
    if let Ok(config) =
        crate::config::Config::load(Path::new("/usr/local/furbo-local/etc/config.json"))
    {
        crate::watchdog::apply(&config.watchdog)?;
    }
    let listener = UnixListener::bind(&socket)?;
    std::fs::set_permissions(&socket, std::fs::Permissions::from_mode(0o660))?;
    chown(&socket, 0, allowed_gid)?;
    let gate = Arc::new(Semaphore::new(1));
    loop {
        let (mut stream, _) = listener.accept().await?;
        let uid = stream.peer_cred()?.uid();
        if uid != allowed_uid && uid != 0 {
            continue;
        }
        let permit = match gate.clone().try_acquire_owned() {
            Ok(p) => p,
            Err(_) => {
                continue;
            }
        };
        tokio::spawn(async move {
            let _permit = permit;
            let result = tokio::time::timeout(Duration::from_secs(35), async {
                let request: Request = read_json(&mut stream, 65536).await?;
                let body = if let Request::Play { length } = &request {
                    ensure!(*length <= MAX, "audio too large");
                    let mut b = vec![0; *length];
                    stream.read_exact(&mut b).await?;
                    b
                } else {
                    vec![]
                };
                execute(request, &body).await
            })
            .await;
            let (h, bytes) = match result {
                Ok(Ok(r)) => (
                    Header {
                        ok: true,
                        length: r.bytes.len(),
                        content_type: r.content_type,
                        value: r.value,
                        error: None,
                    },
                    r.bytes,
                ),
                _ => (
                    Header {
                        ok: false,
                        length: 0,
                        content_type: String::new(),
                        value: serde_json::Value::Null,
                        error: Some("hardware request failed or limit exceeded".into()),
                    },
                    vec![],
                ),
            };
            let _ = tokio::time::timeout(Duration::from_secs(5), async {
                write_json(&mut stream, &h).await?;
                stream.write_all(&bytes).await?;
                Result::<()>::Ok(())
            })
            .await;
        });
    }
}
fn chown(path: &Path, uid: u32, gid: u32) -> Result<()> {
    use std::os::unix::ffi::OsStrExt;
    let p = std::ffi::CString::new(path.as_os_str().as_bytes())?;
    ensure!(
        unsafe { libc::chown(p.as_ptr(), uid, gid) } == 0,
        "chown failed"
    );
    Ok(())
}
async fn run(path: &str, args: &[&str]) -> Result<Vec<u8>> {
    let out = tokio::time::timeout(
        Duration::from_secs(22),
        Command::new(path)
            .args(args)
            .env("PULSE_SERVER", "unix:/var/run/pulse/native")
            .env("PATH", "/usr/sbin:/usr/bin:/sbin:/bin")
            .stdin(std::process::Stdio::null())
            .kill_on_drop(true)
            .output(),
    )
    .await??;
    ensure!(out.status.success(), "factory command failed");
    ensure!(out.stdout.len() < 65536, "unexpected factory output");
    Ok(out.stdout)
}
async fn pic(cmd: &str) -> Result<()> {
    run("/usr/local/bin/cec_pic.sh", &[cmd]).await?;
    Ok(())
}
async fn yuv() -> Result<PathBuf> {
    run(
        "/usr/local/bin/test_yuvcap",
        &["-b", "0", "-Y", "-f", "/run/furbo-hw/frame.yuv"],
    )
    .await?;
    let path = PathBuf::from("/run/furbo-hw/frame.yuv_prev_M_1920x1080.yuv");
    ensure!(
        std::fs::metadata(&path)?.len() >= 1920 * 1080,
        "unexpected YUV frame format"
    );
    Ok(path)
}
fn empty(value: serde_json::Value) -> Reply {
    Reply {
        bytes: vec![],
        content_type: "application/json".into(),
        value,
    }
}
async fn execute(request: Request, body: &[u8]) -> Result<Reply> {
    use serde_json::json;
    match request {
        Request::SetTime { unix_seconds } => {
            ensure!(
                (1_704_067_200..=4_102_444_800).contains(&unix_seconds),
                "invalid time"
            );
            let ts = libc::timespec {
                tv_sec: unix_seconds as _,
                tv_nsec: 0,
            };
            ensure!(
                unsafe { libc::clock_settime(libc::CLOCK_REALTIME, &ts) } == 0,
                "clock update failed"
            );
        }
        Request::AuthState => return Ok(empty(crate::network::auth_state()?)),
        Request::PasswordSetupComplete { generation } => {
            crate::network::complete_password_setup(generation)?
        }
        Request::WifiStatus => return Ok(empty(crate::network::status()?)),
        Request::UpdateConfig { config } => {
            let path = Path::new("/usr/local/furbo-local/etc/config.json");
            let old = crate::config::Config::load(path)?;
            ensure!(
                config.data_dir == old.data_dir
                    && config.hardware_socket == old.hardware_socket
                    && config.device_id == old.device_id
                    && !config.simulate
                    && config.media.directory == old.media.directory,
                "protected device paths cannot change"
            );
            config.validate()?;
            crate::store::atomic_write(path, &serde_json::to_vec_pretty(&config)?)?;
            crate::watchdog::apply(&config.watchdog)?;
            use std::os::unix::ffi::OsStrExt;
            let cpath = std::ffi::CString::new(path.as_os_str().as_bytes())?;
            ensure!(
                unsafe { libc::chown(cpath.as_ptr(), 33, 33) } == 0,
                "cannot set configuration ownership"
            );
            tokio::spawn(async {
                tokio::time::sleep(Duration::from_secs(2)).await;
                let _ = run("/usr/bin/systemctl", &["restart", "furbo-local.service"]).await;
            });
            return Ok(empty(serde_json::json!({"saved":true,"restarting":true})));
        }
        Request::WifiConfigure { ssid, passphrase } => {
            return Ok(empty(crate::network::configure(&ssid, &passphrase)?));
        }
        Request::WifiRecover => crate::network::recover()?,
        Request::WifiConfirm { transaction_id } => {
            return Ok(empty(crate::network::confirm(&transaction_id)?))
        }
        Request::WifiRollback => return Ok(empty(crate::network::rollback()?)),
        Request::Toss => {
            ensure!(clock_valid(), "clock unset; feeding disabled");
            let path = Path::new(WORK).join("last-toss");
            let last = std::fs::read_to_string(&path)
                .ok()
                .and_then(|s| s.parse::<i64>().ok())
                .unwrap_or(0);
            ensure!(now() - last >= 30, "motor cooldown: 30 seconds");
            crate::store::atomic_write(&path, now().to_string().as_bytes())?;
            let result = pic("TF+Ls").await;
            tokio::time::sleep(Duration::from_secs(5)).await;
            let off = pic("TF+LT").await;
            result?;
            off?;
        }
        Request::Led { color } => pic(color.pic()).await?,
        Request::Tunnel { on } => pic(if on { "TF+Lt" } else { "TF+LT" }).await?,
        Request::Overlay {
            label,
            show_timestamp,
        } => {
            ensure!(
                label.len() <= 24 && label.bytes().all(|b| b.is_ascii_graphic() || b == b' '),
                "invalid overlay label"
            );
            let utility = "/usr/bin/test_video_service_dyn_air_api";
            if label.is_empty() {
                run(utility, &["-s", "0", "--overlay-disable", "1"]).await?;
            } else {
                run(
                    utility,
                    &[
                        "-s",
                        "0",
                        "--overlay-data-remove",
                        "1",
                        "--overlay-data-idx",
                        "0",
                    ],
                )
                .await?;
                run(
                    utility,
                    &[
                        "-s",
                        "0",
                        "--overlay-data-add",
                        "1",
                        "--overlay-data-type",
                        "0",
                        "--overlay-w",
                        "240",
                        "--overlay-h",
                        "80",
                        "--overlay-x",
                        "0",
                        "--overlay-y",
                        "0",
                        "--overlay-text-str",
                        &label,
                        "--overlay-font-t",
                        "/usr/share/fonts/FuturaMedium.otf",
                        "--overlay-font-s",
                        "24",
                        "--overlay-font-c",
                        "0",
                    ],
                )
                .await?;
                run(utility, &["-s", "0", "--overlay-enable", "1"]).await?;
            }
            run(
                utility,
                &[
                    "-s",
                    "0",
                    if show_timestamp {
                        "--overlay-enable"
                    } else {
                        "--overlay-disable"
                    },
                    "0",
                ],
            )
            .await?;
            run(utility, &["--overlay-save"]).await?;
            return Ok(empty(
                serde_json::json!({"overlay_text":label,"show_timestamp":show_timestamp}),
            ));
        }
        Request::Night { mode } => {
            return Ok(empty(native_night(&mode).await?));
        }
        Request::Volume { percent } => {
            ensure!(percent <= 75, "speaker limited to 75%");
            run(
                "/usr/bin/pactl",
                &[
                    "set-sink-volume",
                    "alsa_output.platform-sound.6.analog-stereo",
                    &format!("{percent}%"),
                ],
            )
            .await?;
        }
        Request::Mute { muted } => {
            run(
                "/usr/bin/pactl",
                &[
                    "set-sink-mute",
                    "alsa_output.platform-sound.6.analog-stereo",
                    if muted { "1" } else { "0" },
                ],
            )
            .await?;
        }
        Request::Image { setting, value } => {
            ensure!(setting.valid(value), "image value out of range");
            if matches!(setting, ImageSetting::AntiFlicker) {
                crate::store::atomic_write(
                    Path::new("/etc/Flickermode"),
                    value.to_string().as_bytes(),
                )?;
                return Ok(empty(
                    serde_json::json!({"anti_flicker":value,"configured":true}),
                ));
            }
            ensure!(oryx_image_available(),
                "this firmware has no Oryx image service; use native night and anti-flicker controls");
            run(
                "/usr/bin/test_image_service_air_api",
                &[setting.option(), &value.to_string()],
            )
            .await?;
        }
        Request::Snapshot => {
            let yuv = yuv().await?;
            let result = run(
                "/usr/local/bin/jpg_enc",
                &[
                    "-v",
                    "-y",
                    yuv.to_str().unwrap(),
                    "-w",
                    "1920",
                    "-h",
                    "1080",
                    "-q",
                    "80",
                    "-f",
                    "/run/furbo-hw/frame.jpg",
                ],
            )
            .await;
            let _ = std::fs::remove_file(yuv);
            result?;
            let bytes = read_bounded(Path::new("/run/furbo-hw/frame.jpg"))?;
            let _ = std::fs::remove_file("/run/furbo-hw/frame.jpg");
            ensure!(bytes.starts_with(&[0xff, 0xd8]), "invalid JPEG");
            return Ok(Reply {
                bytes,
                content_type: "image/jpeg".into(),
                value: json!({}),
            });
        }
        Request::Record { seconds } => {
            ensure!(
                (1..=15).contains(&seconds),
                "recording must be 1..15 seconds"
            );
            run(
                "/usr/bin/arecord",
                &[
                    "-q",
                    "-t",
                    "wav",
                    "-f",
                    "S16_LE",
                    "-c",
                    "1",
                    "-r",
                    "16000",
                    "-d",
                    &seconds.to_string(),
                    "/run/furbo-hw/voice.wav",
                ],
            )
            .await?;
            let bytes = read_bounded(Path::new("/run/furbo-hw/voice.wav"))?;
            let _ = std::fs::remove_file("/run/furbo-hw/voice.wav");
            validate_wav(&bytes)?;
            return Ok(Reply {
                bytes,
                content_type: "audio/wav".into(),
                value: json!({}),
            });
        }
        Request::Play { length } => {
            ensure!(length == body.len(), "bad audio body");
            validate_wav(body)?;
            let path = Path::new("/run/furbo-hw/play.wav");
            crate::store::atomic_write(path, body)?;
            let r = run("/usr/bin/aplay", &["-q", path.to_str().unwrap()]).await;
            let _ = std::fs::remove_file(path);
            r?;
        }
        Request::Lux => {
            let raw = run(
                "/usr/local/bin/i2cget",
                &["-f", "-y", "2", "0x44", "0x00", "w"],
            )
            .await?;
            let s = std::str::from_utf8(&raw)?.trim();
            let v = u16::from_str_radix(s.trim_start_matches("0x"), 16)?;
            let v = v.swap_bytes();
            let lux = 0.01 * ((1u32 << ((v >> 12) & 15)) as f64) * ((v & 0xfff) as f64);
            return Ok(empty(
                json!({"lux":lux,"ir_enabled":std::fs::read_to_string("/sys/class/gpio/gpio108/value").ok().map(|s|s.trim()=="1")}),
            ));
        }
        Request::MotionSample => {
            let p = yuv().await?;
            let bytes = std::fs::read(&p)?;
            let _ = std::fs::remove_file(p);
            let mut sample = Vec::with_capacity(160 * 90);
            for y in 0..90 {
                for x in 0..160 {
                    sample.push(bytes[(y * 12) * 1920 + x * 12]);
                }
            }
            return Ok(Reply {
                bytes: sample,
                content_type: "application/octet-stream".into(),
                value: json!({}),
            });
        }
        Request::SoundSample => {
            run(
                "/usr/bin/arecord",
                &[
                    "-q",
                    "-t",
                    "raw",
                    "-f",
                    "S16_LE",
                    "-c",
                    "1",
                    "-r",
                    "16000",
                    "-d",
                    "1",
                    "/run/furbo-hw/level.raw",
                ],
            )
            .await?;
            let bytes = read_bounded(Path::new("/run/furbo-hw/level.raw"))?;
            let _ = std::fs::remove_file("/run/furbo-hw/level.raw");
            let n = bytes.len() / 2;
            ensure!(n > 0, "empty audio");
            let rms = (bytes
                .as_chunks::<2>()
                .0
                .iter()
                .map(|b| {
                    let v = i16::from_le_bytes([b[0], b[1]]) as f64 / 32768.0;
                    v * v
                })
                .sum::<f64>()
                / n as f64)
                .sqrt();
            return Ok(empty(json!({"rms":rms})));
        }
    }
    Ok(empty(json!({"ok":true})))
}
fn read_bounded(path: &Path) -> Result<Vec<u8>> {
    ensure!(
        std::fs::metadata(path)?.len() <= MAX as u64,
        "media too large"
    );
    Ok(std::fs::read(path)?)
}
pub fn validate_wav(b: &[u8]) -> Result<()> {
    ensure!(
        b.len() >= 44 && b.len() <= MAX && &b[..4] == b"RIFF" && &b[8..12] == b"WAVE",
        "expected PCM WAV"
    );
    let mut i = 12;
    let mut format = None;
    let mut data = None;
    while i + 8 <= b.len() {
        let n = u32::from_le_bytes(b[i + 4..i + 8].try_into()?) as usize;
        ensure!(n <= b.len() - i - 8, "truncated WAV chunk");
        if &b[i..i + 4] == b"fmt " {
            ensure!(n >= 16, "bad WAV format");
            let u = |o| u16::from_le_bytes([b[i + 8 + o], b[i + 9 + o]]);
            let rate = u32::from_le_bytes(b[i + 12..i + 16].try_into()?);
            ensure!(
                u(0) == 1
                    && (1..=2).contains(&u(2))
                    && u(14) == 16
                    && (8000..=48000).contains(&rate),
                "only PCM16 mono/stereo 8..48kHz"
            );
            format = Some((rate as usize, u(2) as usize));
        }
        if &b[i..i + 4] == b"data" {
            data = Some(n);
        }
        i += 8 + n + (n & 1);
    }
    let (rate, channels) = format.context("missing WAV fmt")?;
    let size = data.context("missing WAV data")?;
    ensure!(
        size > 0 && size % (2 * channels) == 0 && size <= rate * channels * 2 * 15,
        "audio must be at most 15 seconds"
    );
    Ok(())
}
pub fn silent_wav(seconds: u8) -> Vec<u8> {
    let n = 16000 * 2 * seconds as u32;
    let mut b = vec![];
    b.extend(b"RIFF");
    b.extend((36 + n).to_le_bytes());
    b.extend(b"WAVEfmt ");
    b.extend(16u32.to_le_bytes());
    b.extend(1u16.to_le_bytes());
    b.extend(1u16.to_le_bytes());
    b.extend(16000u32.to_le_bytes());
    b.extend(32000u32.to_le_bytes());
    b.extend(2u16.to_le_bytes());
    b.extend(16u16.to_le_bytes());
    b.extend(b"data");
    b.extend(n.to_le_bytes());
    b.resize(44 + n as usize, 0);
    b
}
fn simulated(request: Request, body: &[u8]) -> Result<Reply> {
    use serde_json::json;
    match request {
        Request::AuthState => Ok(empty(
            json!({"setup_required":false,"generation":null,"recovery_active":true,"simulated":true}),
        )),
        Request::Record { seconds } => {
            ensure!((1..=15).contains(&seconds), "invalid seconds");
            Ok(Reply {
                bytes: silent_wav(seconds),
                content_type: "audio/wav".into(),
                value: json!({"simulated":true}),
            })
        }
        Request::Play { .. } => {
            validate_wav(body)?;
            Ok(empty(json!({"simulated":true})))
        }
        Request::Volume { percent } => {
            ensure!(percent <= 75, "volume too high");
            Ok(empty(json!({"simulated":true})))
        }
        Request::Image { setting, value } => {
            ensure!(setting.valid(value), "invalid image setting");
            Ok(empty(json!({"simulated":true})))
        }
        Request::Snapshot => {
            let bytes = base64::engine::general_purpose::STANDARD
                .decode(include_str!("../tests/frame.b64").trim())?;
            Ok(Reply {
                bytes,
                content_type: "image/jpeg".into(),
                value: json!({"simulated":true}),
            })
        }
        Request::Lux => Ok(empty(
            json!({"lux":42.0,"ir_enabled":false,"simulated":true}),
        )),
        Request::MotionSample => Ok(Reply {
            bytes: vec![100; 14400],
            content_type: "application/octet-stream".into(),
            value: json!({}),
        }),
        Request::SoundSample => Ok(empty(json!({"rms":0.0}))),
        _ => Ok(empty(json!({"simulated":true}))),
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn wav_limits() {
        assert!(validate_wav(&silent_wav(2)).is_ok());
        assert!(validate_wav(&silent_wav(16)).is_err());
        assert!(validate_wav(b"RIFFbroken").is_err());
        let mut b = silent_wav(1);
        b[20] = 3;
        assert!(validate_wav(&b).is_err());
    }
}
