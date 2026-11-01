# Deployment and recovery

Target: an owned, rooted Furbo2 with the inspected Ambarella S2L firmware. Keep the pre-install backup and pinned SSH details in ignored local storage.

## File layout

```text
/usr/local/furbo-local/bin/furbo-local
/usr/local/furbo-local/etc/config.json
/usr/local/furbo-local/data/state.json
/usr/local/furbo-local/data/jwt-secret
/usr/local/furbo-local/data/server.crt
/usr/local/furbo-local/data/server.key
/usr/local/furbo-local/data/broker-ca.crt           optional
/usr/local/furbo-local/network/recovery.json
/usr/local/furbo-local/network/station.conf        last confirmed profile
/usr/local/furbo-local/network/station.pending     unconfirmed trial
/usr/local/furbo-local/network/pending.json        boot ID + monotonic deadline
/usr/local/furbo-local/network/password-reset-required
/dev/adc/furbo-local/media/
/run/furbo-local/hardware.sock
/run/furbo-local/auth-reset.json
/run/furbo-network/
```

The network directory is root-only. Data and the main JSON are owned by the unprivileged daemon, mode 0700 for directories and 0600 for files. The helper socket is root-owned, GID 33, mode 0660, and additionally checks peer UID.

On the inspected image, UID 33 is www-data and GID 33 is named cdrom. The unit deliberately uses numeric GID 33.

## Startup

The existing local boot helper must start key-only SSH first. Replace its legacy Wi-Fi watchdog exec with a start of the supervised network service only after a successful live test.

```sh
exec /usr/bin/systemctl start furbo-network.service
```

Keep the original helper privately. Do not enable competing station managers. The boot hook starts furbo-network.service so systemd supervises and restarts the network process. Keep that unit disabled for automatic target startup: the existing hook starts it after factory initialization.

The network unit lowers the interface and unloads cywdhd during shutdown, matching the factory safe-reboot sequence. This avoids leaving the radio firmware running across a warm reboot.

Enable furbo-hardware.service and furbo-local.service through systemd. The API unit needs the factory camera/audio services but does not require the vendor cloud manager.

The service's mounted media directory should be created only after verifying the JFFS2 data filesystem is mounted. Remove the factory automatic erase-on-missing-marker branches before using this filesystem.

## Initialization

```sh
furbo-local init /usr/local/furbo-local/etc/config.json
furbo-local network-init
```

Initialization refuses to overwrite an existing state database. New state has no administrator password; initial setup runs on the recovery network. Existing state and JWT secrets must survive upgrades.

If adopting an existing installation, preserve its working root-only WPA profile as network/station.conf before transferring network ownership.

The media engine's automatic MP4 file writer should be disabled. This prevents hidden recordings from exhausting /tmp RAM; the RTSP engine remains available.

## Remote network testing

Stage an independent timed recovery before changing a live radio:

1. Save the confirmed profile and current boot helper privately.
2. Start a detached rollback that can stop the test network manager, reload the cywdhd driver in station mode, and restore the original known-good watchdog/profile.
3. Trigger recovery and inspect AP/WPA2/DHCP/HTTPS.
4. Confirm a valid trial, test an unconfirmed timeout, and test an unconfirmed reboot.
5. Remove the test rollback after success.

A WPA2 AP requires at least an eight-character password. The intentional setup default is furbo2setup, not the shorter SSID.

## Reset semantics

The GPIO 53 handler arms only after observing a released button and requires a continuous ten-second press. This avoids acting on an unknown/stuck startup level.

A reset removes the confirmed/trial Wi-Fi profiles, including the legacy active local profile, and records a persistent password-reset generation. Authentication is blocked immediately. Password setup rotates the JWT secret and replaces local accounts/API keys. The reset marker is cleared only after setup is durable.

No filesystem is formatted. Media, application files, MQTT JSON, and SSH keys remain intact.

Root administrators can invoke `furbo-local factory-reset` for the same operation; it is not exposed as an unauthenticated API action.

## Time

The network manager starts the existing BusyBox NTP client after station Wi-Fi connects. Configure up to four NTP hostnames/IPs in network/recovery.json (ntp_servers); the defaults are time.cloudflare.com and time.google.com. Use a trusted LAN time server where available, or an empty list to disable this client. No NTP server listener is enabled.

Use the authenticated time endpoint/UI or pinned SSH if automatic synchronization is unavailable. API-key authentication can bootstrap the time endpoint before JWTs/outbound TLS are usable. Never solve an invalid clock by disabling certificate verification.

The application embeds current timezone data; the factory journal's local timezone display can still be old.

## Roll back the application

Stop the API/helper/network services. Restore the saved boot helper and the last confirmed station profile. Restart the verified original Wi-Fi watchdog. Preserve the SSH startup hook and its host key.

Do not remotely uninstall the SSH boot path or re-enable the vendor network manager without an available recovery method.

The repository does not contain a flash partition restore procedure.


## Configurable factory watchdog

Install furbo-watchdog.service and replace the original unmanaged ProcWatchdog.sh launch in cec_init.sh with systemctl start furbo-watchdog.service. Keep its existing crash/radio checks intact. At the beginning of its monitoring loop, load the root-only numeric threshold file when present:

```sh
if [ -r /run/furbo-local/watchdog.env ]; then
    . /run/furbo-local/watchdog.env
fi
```

The root helper generates that file from config.json; the shell file is not a separate source of configuration. It is mode 0600, contains only validated integers, and updates without restarting the monitor. CPU defaults are 50% for audio and RTSP, with 24 consecutive high readings. The factory counter compares with greater-than; the generated maximum is therefore one less than the configured reading count. Settings are administrator-only through the API/UI; MQTT CONFIGURE uses the same validation.

Preserve original scripts privately. Run only one watchdog instance. Move the existing monitor under the service once during installation; do not enable a competing automatic startup path.
