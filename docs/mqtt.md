# MQTT

All topics are inside `mqtt.topic_prefix`; a trailing slash is normalized. The firmware does not append the device ID to this root.

This uses the Tasmota FullTopic convention `%topic%/%prefix%/`, preserving a single broker ACL namespace.

| Direction | Topic |
| --- | --- |
| Client → device | ROOT/cmnd/COMMAND |
| Device → client | ROOT/stat/RESULT |
| Client → device | ROOT/cmnd/LED, ROOT/cmnd/STATS, ROOT/cmnd/WIFI, etc. |
| Device → client | ROOT/stat/LED, ROOT/stat/STATS, ROOT/stat/WIFI, etc. |
| Device telemetry | ROOT/tele/STATE |
| Event notifications | ROOT/tele/EVENT |
| Retained availability / last will | ROOT/tele/LWT: Online or Offline |

For a configured root such as `furbo/device`, the command/ack pair is `furbo/device/cmnd/LED` and `furbo/device/stat/LED`.

## Authentication and TLS

Configure host, port, username, password and root in the device's protected JSON. A password may instead be read from password_file; do not set both.

- `tls: true` enables verified Rustls TLS.
- `ca_file` adds a private CA or self-signed broker certificate.
- The connection host is verified by default.
- If connecting by IP to a certificate with DNS SANs, `certificate_name` selects the expected certificate identity. The full chain, validity period and signature remain verified.
- Optional client_certificate and client_key enable mutual TLS.
- No skip-verification switch is provided.

The clock must be set before outbound TLS. The client waits and reconnects after network changes.

## Commands and acknowledgments

A named command's JSON contains a unique ID, current UNIX timestamp, and action fields:

```json
{"id":"request-123","issued_at":0,"color":"green"}
```

Publish that to `ROOT/cmnd/LED`, replacing 0 with the current UNIX seconds. Receive `ROOT/stat/LED`:

```json
{"id":"request-123","ok":true,"result":{}}
```

The result's fields depend on the action. Failed requests return `ok: false` and an error. Correlate responses by ID.

Supported named actions:

```text
TOSS LED TUNNEL NIGHT VOLUME MUTE IMAGE CAPTURE RECORD VIDEO PLAY UPLOAD
WIFI WIFICONFIRM WIFIROLLBACK SCHEDULE DELETE EVENT
```

Use the action fields documented in [the API](api.md), omitting type. SCHEDULE takes a schedules array; DELETE takes media_id; EVENT takes kind and details. Read requests STATUS, STATS, WIFISTATUS, SCHEDULES and MEDIA take ID and issued_at and return their matching stat topics.

MEDIA accepts optional offset and limit (at most 32) and returns items plus total. SCHEDULES returns the saved feed-time array.

Alternatively publish the complete API command envelope to `ROOT/cmnd/COMMAND` and read `ROOT/stat/RESULT`.

Commands use QoS 1, clean sessions, bounded packets/queues, an age limit, and persistent ID deduplication. Do not retain commands. Retained commands received on subscription are ignored. Reconnects do not intentionally replay commands queued while the device was offline.

Telemetry and last will are retained. Event notifications and acknowledgments are not retained. Telemetry is best effort; webhook/media queues provide the durable retry path.

## Wi-Fi over MQTT

1. Publish to `ROOT/cmnd/WIFI`:

   ```json
   {"id":"wifi-change-1","issued_at":0,"ssid":"new-network","passphrase":"new-password"}
   ```

2. Read `ROOT/stat/WIFI`. Its result contains the transaction ID and 300-second confirmation window.
3. After the device reconnects, query `ROOT/cmnd/WIFISTATUS`.
4. Publish to `ROOT/cmnd/WIFICONFIRM`:

   ```json
   {"id":"wifi-confirm-1","issued_at":0,"transaction_id":"the-returned-id"}
   ```

5. Require `ok: true` and `confirmed: true` in `ROOT/stat/WIFICONFIRM`.

Explicit confirmation can instead use the API. Association alone never commits the new profile. A timeout or unconfirmed reboot restores the old profile. WIFIROLLBACK explicitly aborts the trial.

Wi-Fi passwords remain in memory, the TLS command transport, and root-only generated profiles; they are not echoed in events/acknowledgments.

## Broker ACL

Give the device account publish, receive, subscribe and unsubscribe permissions only inside `ROOT/#`. An exact `ROOT` ACL does not include its subtopics. Do not grant global `#` or a shared broad device role.

The runtime publishes no discovery messages outside the configured root.


## Administrator configuration

cmnd/SETTINGS returns the same redacted settings view as GET /api/v1/settings on stat/SETTINGS.

cmnd/CONFIGURE accepts id, issued_at and settings. Acknowledgments use stat/CONFIGURE. For example (replace the timestamp):

```json
{"id":"watchdog-update","issued_at":0,"settings":{"watchdog":{"audio_cpu_percent":50,"rtsp_cpu_percent":50,"consecutive_readings":24}}}
```

The settings object also accepts complete detection, media, mqtt, http and camera sections from SETTINGS, plus timezone and write-only credential/PEM fields. This covers motion/sound thresholds and callback/upload URLs. All traffic remains inside the configured topic root. Saving configuration briefly restarts the API/MQTT service; wait for Online before issuing further commands.

Motion/sound notifications use tele/EVENT and the configured signed event callback. Captures can additionally be uploaded to the media endpoint.
