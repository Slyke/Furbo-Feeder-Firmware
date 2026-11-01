# Furbo Local

Local control for an owned Furbo2: a small Rust daemon, authenticated API, browser interface, and MQTT client. The vendor phone app and cloud manager are not required.

This is a control layer on the existing Ambarella Linux firmware. It uses the factory ISP, hardware encoder, audio services, and a few fixed hardware commands. It is not a NAND image or a replacement camera driver.

## Features

- Independent HTTP and HTTPS listeners; either can be disabled. Reset restores both recovery listeners. TLS certificates can be locally signed.
- Local accounts with viewer, operator, and administrator roles; Argon2id passwords, one-hour HS256 JWTs, revocable API keys.
- MQTT over verified TLS, custom trust certificates, optional remembered self-signed trust, and optional client certificates.
- Tasmota-style command, acknowledgment, and telemetry topics inside one configurable namespace.
- Native 1080p H.264/AAC RTSP-to-fragmented-MP4 streaming. No FFmpeg or re-encoding on the device.
- JPEG snapshots, bounded video clips, microphone recordings, uploaded voice messages, and speaker playback.
- Treat tosses, recurring feed times, indicator colors, tunnel lighting, night vision, speaker level/mute, and image settings.
- Staged Wi-Fi changes through the API or MQTT. Explicit confirmation within five minutes; timeout or reboot restores the last confirmed network.
- Protected recovery Wi-Fi, a one-time password setup page, and a credential-clearing reset-button handler.
- Authenticated hardware/resource statistics: MAC, factory serial, device UUID, kernel/CPU, RAM, filesystem capacity/free space, and network counters.
- Configurable age/free-space media retention, HTTP media uploads, signed webhook notifications, MQTT event notifications, and retry queues.
- Optional local luma-change motion detection and audio-level detection. External authenticated events can supply dog, person, or bark classifications.

Proprietary Furbo cloud recognition and subscription services are not reproduced. Audio-level detection is not a trained bark classifier. Browser talk sends a recorded PCM voice message after recording stops; live room audio accompanies the video stream.

## Hardware fit

The inspected S2L ARMv7 device exposes about 105 MiB of Linux RAM, a 92 MiB root filesystem, and a separate 120 MiB JFFS2 filesystem. A static musl build is roughly 5 MiB, including the UI and timezone data. The API has been measured at a few MiB of resident RAM.

Factory MP4 recording can fill a RAM-backed temporary filesystem. Disable that file writer while retaining the media/RTSP engine. See [hardware notes](docs/hardware.md) for the relevant checks and the boot-script erase guard.

## Build and try locally

Rust stable is required. Development runs default to simulation, which does not access hardware.

```sh
cp config.example.json config.json
cargo build
cargo run -- init config.json
scripts/gen-certs.sh data localhost 127.0.0.1
cargo run -- serve config.json
```

Open `https://localhost:8443`. On a simulated localhost installation, the first visit creates the administrator password. The setup endpoint closes once a password has been configured.

Administrator connection/event settings are editable in the dashboard and through MQTT CONFIGURE. Passwords and private keys are never returned by the settings API. The runtime configuration is **JSON**. `Cargo.toml` and `.cargo/config.toml` are Rust build metadata.

For an ARMv7 static binary, install Clang and LLVM archive tools:

```sh
rustup target add armv7-unknown-linux-musleabihf
cargo build --release --target armv7-unknown-linux-musleabihf
```

The cross-build wrappers select a local Clang and LLVM `ar`. No recent glibc is required on the device.

## Install on the device

Start with working, pinned, key-only SSH and a private backup. This repository does not provide an initial root exploit.

1. Prepare an ignored private device JSON file from the example. Set `simulate` to `false`, use the paths in `deploy/furbo-local.service`, and set the timezone and MQTT options.
2. Stage the static executable, JSON, HTTPS certificate/key, optional broker CA, and the systemd units using encrypted SSH stdin. This firmware's Dropbear may lack SCP and SFTP.
3. Run `init` only for a new data directory. Preserve an existing `state.json` and `jwt-secret`.
4. Run `network-init` once, and preserve any existing station profile as `/usr/local/furbo-local/network/station.conf`.
5. Run the hardware helper as root. Run the web/MQTT daemon as UID/GID 33. The provided units target the inspected systemd 215 image.
6. Make the Wi-Fi manager the only station/AP owner. Keep the SSH startup path intact. See [deployment notes](docs/deployment.md).

Do not copy live credentials into the public example. The installed configuration should be `/usr/local/furbo-local/etc/config.json`, mode `0600`, readable by the daemon account. Keep network profiles and reset markers root-only.

## First setup and reset

When no confirmed network is available, the recovery network is:

- SSID: **furbo2**
- Default WPA2 password: **furbo2setup**
- Setup page: **https://192.168.42.1:8443/**

Recovery DNS and HTTP direct clients to the HTTPS page. Trust the device certificate on your client.

Hold the rear reset button for **10 seconds** while the local network manager is running. It clears saved Wi-Fi and invalidates local sign-in credentials. The next setup creates a new administrator password and revokes old local accounts, JWTs, and API keys. It also restores HTTP/HTTPS on all interfaces at ports 8080/8443. Firmware, recordings, feed schedules, SSH keys, and MQTT configuration are retained. Feed/detection jobs pause while password setup is required.

Password creation is accepted only from the recovery subnet while recovery is active, or from localhost in simulation. It cannot be used again after setup is complete.

A manual `network/recover` request starts recovery without clearing the configured password.

## Wi-Fi changes

A change is a transaction:

1. Submit the new SSID and WPA2 passphrase/PSK.
2. Receive a transaction ID before the connection changes.
3. Connect on the trial network.
4. Confirm that transaction through the API or MQTT within **300 seconds**.

Association alone never commits the credentials. A failed trial returns to the previous network; an unconfirmed reboot discards the trial. When no previous network exists, rollback returns to recovery.

See [API](docs/api.md) and [MQTT](docs/mqtt.md) for requests and acknowledgment topics.

## Time and storage

The inspected device has no reliable current clock after power loss. The network manager starts a small NTP client after Wi-Fi connects. Time servers are configurable in root-only network/recovery.json; an empty list disables automatic synchronization. TLS clients, JWT login, detection, and scheduled feeds wait for a plausible clock. An administrator can also synchronize it from the UI or authenticated time API; `scripts/sync-time.sh` can set it through pinned SSH. Password setup can set time from the browser.

Feed times use bundled IANA tzdb **2026f**, independently of the old firmware's timezone files. Missed feeds are skipped, and a repeated local minute runs once per date.

Media has byte and file quotas, optional age expiry, and a minimum free-space reserve. Oldest unprotected files are removed first. Pending uploads and scheduled voice messages are protected from retention eviction; capture fails if protected files exhaust the quota. Video clips are capped at 30 seconds and 4 MiB. Downloaded clips carry finite duration metadata and a keyframe index for seeking. At most two video streams run concurrently; a stream reconnects every five minutes.

The UI supports English and Simplified Chinese from JSON language files, with an optional initial ?lang=en-US or ?lang=zh-CN selection. Page navigation uses /overview, /recordings, /feed-times, /settings, /connections and /accounts. Light/dark selection and language preference are saved in the browser. A compact, locally served Chinese font subset is included under its [OFL license](web/fonts/OFL.txt), derived from [Noto Sans CJK](https://github.com/notofonts/noto-cjk). Login survives page refresh within the current browser tab; signing out clears that session. The administrator can download a RAM-only NDJSON log (256 KiB maximum), cleared by a service restart.

The recovery watchdog defaults to 50% CPU per audio/RTSP process and 24 consecutive high readings. Administrators can change these values through settings or MQTT; crash/radio-fault recovery stays active.

## Validation

```sh
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
cargo build --release --target armv7-unknown-linux-musleabihf
node --check web/app.js
```

An optional GitHub Actions template is in [deploy/github-actions-check.yml](deploy/github-actions-check.yml). Copy it to .github/workflows/check.yml to enable hosted checks.

Hardware validation is separate from simulation. A treat toss moves a motor; media tests capture the room. Keep private test outputs out of Git.

## Private material

`local.readme.md`, `local-docs/`, runtime data/configuration, certificates/keys, and captured media are ignored. The public [hardware handoff](docs/hardware.md) contains only general platform facts and command interfaces.

## License

New code is MIT licensed. Bundled timezone data carries its own public-domain notice in `tzdb/LICENSE`. Factory firmware and vendor binaries are not distributed.
