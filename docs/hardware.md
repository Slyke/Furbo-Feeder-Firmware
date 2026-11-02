# Sanitized Furbo2 hardware handoff

This file deliberately omits network addresses, Wi-Fi profiles, MAC/serial values, SSH fingerprints, private-key locations, customer media, and device-specific backup hashes. Keep those in ignored local documentation.

## Inventory

| Component | Observed interface |
| --- | --- |
| Platform | Ambarella S2L, ARMv7 Cortex-A9, hard-float ABI |
| Kernel / libc | Linux 3.10.73+, glibc 2.19 factory userland |
| Linux RAM | 107,256 KiB visible; no swap |
| Root filesystem | Approximately 92 MiB UBIFS |
| Data filesystem | Approximately 120 MiB JFFS2 at /dev/adc |
| Camera | AR0237, /dev/iav, 1920×1080 JPEG and H.264 |
| Audio | RT5670 codec; PulseAudio owns ALSA hardware |
| Wireless | cywdhd / wlan0; managed and AP modes advertised |
| Ambient light | OPT3001-compatible sensor, I2C bus 2, address 0x44 |
| IMU | MPU6500 enumerates on I2C; motion-value testing remains separate |
| Reset key | GPIO 53; inspected idle level 0 |
| IR enable | GPIO 108 and PWM backlight interface |

The static service targets `armv7-unknown-linux-musleabihf`. Keep native video encoding; software transcoding is unnecessary.

## Keep the hardware stack

The vendor cloud manager must not reclaim the network. The local implementation still requires the installed camera/audio stack, including `cec_image`, `apps_launcher`, `media_svc`, `video_svc`, `audio_svc`, `rtsp_svc`, and PulseAudio. Remove unused cloud app/update files only after backup and dependency checks.

The internal RTSP server listens on loopback port 554. Its SDP identifies H.264 video and 16 kHz mono AAC. The Rust adapter negotiates RTSP/TCP, depacketizes RTP, and muxes fragmented MP4.

## Non-destructive interfaces

| Function | Factory interface used |
| --- | --- |
| RGB indicator | cec_pic.sh with TF+L0/1/2/3/4/5/7/9 |
| Tunnel lamp | TF+Lt on; TF+LT off |
| One treat cycle | TF+Ls; tunnel lamp is turned off afterward |
| Night vision | Native cec_image Unix socket; verified GPIO 108/PWM state |
| Image settings | Native night and anti-flicker; other tuning requires an active Oryx image service |
| Snapshot | test_yuvcap + jpg_enc; helper uses its own private temporary workspace |
| Microphone | arecord through the PulseAudio-backed default ALSA device |
| Speaker | aplay; volume/mute through the PulseAudio sink |
| Ambient light | Fixed I2C read; SMBus word is byte-swapped before decoding |

PIC scripts temporarily set ttyS0 to 9600 baud and restore 115200. They can interfere with a simultaneous serial-console session.

The factory PIC helper is a write-only UART path: it transmits the command but does not read a reply or expose a motor-position sensor. A successful toss therefore confirms only that both UART helper invocations exited successfully, not that a treat physically moved. The hardware service records each helper's exit code, elapsed time, bounded stdout/stderr, and whether the UART write was accepted. It reports `controller_acknowledged: false` and `physical_completion_verified: false` rather than inventing an acknowledgment.

The helper runs under a fail-fast shell so a failed baud change or UART write cannot be hidden by the final baud-restore command. Treat commands are never retried automatically: a retry after an uncertain result could produce a double toss.

The native speaker path is `alsa_output.platform-sound.6.analog-stereo`. The service restores a persisted 25% default and caps normal API volume at 75%.

The old PWM driver's accepted values do not match every documented duty value. Use the native cec_image control socket rather than writing arbitrary PWM levels. Its night enum is 0 auto, 1 off, 2 on. The Oryx test_image_service_air_api returned success without changing this device because its image service is absent.

## Storage and boot

The factory `Load_adc_partition` function can erase a data partition when its `checked` marker is absent. Remove the automatic erase branches before relying on that filesystem. Mount the filesystem normally and preserve the original script privately.

The local reset handler never formats a filesystem or accesses raw MTD. Its credential deletion is limited to local network profiles and authentication reset state.

Factory MP4 muxers may automatically write into RAM-backed `/tmp/video`. Disable `auto_file_writing` in the MP4 muxer configurations and stop file writing through the media API. Keep the recording engine and RTP muxer active. Back up existing recordings privately before deleting them.

Do not use nandwrite, flash_eraseall, MTD erase operations, or a partition restore as part of this application installation.

## Access and recovery

Use a dedicated SSH configuration entry with a verified host key. The inspected Dropbear setup is key-only and may have SFTP, SCP, and port forwarding disabled. Transfer binary data through SSH stdin/stdout; do not pass it through a text conversion.

Preserve the existing local SSH/Wi-Fi boot helper until the replacement manager is verified. The camera services can take about 2.5 minutes to initialize after a cold boot.

A socket-activated system journal gateway may expose port 19531 without authentication. Mask its socket and service when it is not needed; merely disabling the socket did not survive a boot dependency.

UART recovery uses 3.3 V TTL, crossed TX/RX, and common ground. Do not connect adapter power to the board. Do not reseat internal flex cables while powered.
