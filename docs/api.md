# API

Base path: `/api/v1`. Requests and responses are JSON unless an endpoint transfers media. Authenticate with either:

```text
Authorization: Bearer <JWT-or-API-key>
X-API-Key: <API-key>
```

Do not supply both. Tokens are not accepted in query strings. No cookie authentication or permissive CORS is enabled.

## Public endpoints

| Method / path | Purpose |
| --- | --- |
| GET /healthz | Minimal process health |
| GET /api/v1/setup/status | Whether initial/reset password creation is required |
| POST /api/v1/setup/password | Create the administrator password once; recovery clients only |
| POST /api/v1/login | Username/password to 30-day JWT |

Setup body:

```json
{"password":"choose-your-own-long-password","unix_seconds":0}
```

Replace 0 with the current UNIX time, or omit that optional field. Setup requires the active recovery subnet, or simulated localhost. Once complete, repeated calls return 409. A reset invalidates existing sign-in credentials before allowing a new setup.

Login body is `{"username":"admin","password":"..."}`. When the device clock is unset, login returns a five-minute restricted setup token for network/time repair. After setting time, `POST /session/upgrade` exchanges it for a normal JWT.

## Authenticated endpoints

| Method / path | Access | Purpose |
| --- | --- | --- |
| GET /me | Viewer | Current username/role |
| POST /logout | Viewer | Revoke this account's JWT sessions |
| GET /status | Viewer | Current control state and capabilities |
| GET /settings | Admin | Configuration with secrets redacted |
| PUT /settings | Admin | Save connection/event/retention/camera/watchdog settings; restart service |
| GET /logs.ndjson | Admin | Download bounded volatile application log |
| GET /stats | Viewer | MAC, factory serial/UUID, CPU/kernel, RAM, disks and network counters |
| GET /sensors | Viewer | Ambient lux and IR enable state |
| GET /events | Viewer | Recent bounded event history |
| GET /media | Viewer | Media IDs, types, sizes and creation times |
| GET /media/{id} | Viewer | Authenticated binary download |
| GET /stream?audio=false | Viewer | Native H.264 fragmented-MP4 stream; opt in to AAC room audio |
| POST /commands | Operator | Execute a typed action |
| POST /listen | Operator | Temporary 1–5 second microphone WAV, not saved |
| PUT /voice | Operator | Upload PCM16 WAV, at most 15 seconds and 1.5 MB |
| DELETE /media/{id} | Operator | Remove media not referenced by a feed schedule |
| GET /schedules | Viewer | Recurring feed times |
| PUT /schedules | Operator | Replace the schedule array |
| POST /events | Operator | Submit an external motion/sound/bark/person/dog/activity/selfie event |
| POST /time | Admin | Set device time using unix_seconds |
| GET /network | Admin | Connection/trial status |
| PUT /network | Admin | Stage SSID and passphrase |
| POST /network/confirm | Admin | Confirm transaction_id while connected on the trial |
| POST /network/rollback | Admin | Return to the last confirmed network |
| POST /network/recover | Admin | Start recovery without clearing accounts |
| GET, POST /users | Admin | List/create local accounts |
| PUT /users/{username} | Admin | Change password, role, or disabled state |
| GET, POST /keys | Admin | List/create API keys |
| DELETE /keys/{id} | Admin | Revoke an API key |

Stats are JSON values in bytes/seconds rather than preformatted display strings. Missing hardware values are null. On the old kernel, available RAM is an explicitly marked estimate when MemAvailable is absent. Disk free space comes from statvfs and is separate from the configured media quota.

## Typed commands

```json
{
  "id": "unique-request-id",
  "issued_at": 0,
  "action": {"type":"led","color":"green"}
}
```

Set `issued_at` to the current UNIX timestamp. Commands default to a 60-second age limit. Reuse the same ID after a connection failure; the saved result is returned. IDs are reserved durably before a side effect, so an interrupted toss is not repeated automatically.

| type | Fields |
| --- | --- |
| toss | None; one factory-defined cycle |
| led | color: off, red, yellow, green, cyan, blue, purple, rgb |
| tunnel | on: boolean |
| night | mode: off, on, auto |
| volume | percent: 0–75 |
| mute | muted: boolean |
| image | setting and integer value |
| capture | upload: boolean |
| record | seconds: 1–15; upload: boolean |
| video | seconds: 1–30; upload: boolean; records video and room audio |
| play | media_id of a stored PCM WAV |
| upload | media_id; queues the configured binary upload |
| configure | settings: same update object as PUT /settings; administrator only |
| set_schedules | schedules: array; same validation as PUT /schedules |
| delete_media | media_id, not referenced by a schedule |
| external_event | kind and details, at most 4 KiB |
| wifi_stage | ssid and passphrase; administrator only |
| wifi_confirm | transaction_id; administrator only |
| wifi_rollback | No fields; administrator only |

The inspected firmware supports native day_night (0 day/off, 1 night/on) and anti_flicker (0 for 50 Hz, 1 for 60 Hz). Other adjustments require an active Oryx image service and are disabled in the UI when it is absent; requests fail clearly rather than reporting a no-op as success. See status.capabilities.image_controls.

On firmware with that service, image values are -2…2 for brightness, contrast, saturation, sharpness and hue; 0/1 for day_night and anti_flicker; 0…9 for white_balance.

Motor cooldown is 30 seconds. Commands are serialized and limited to 64 per minute. Unknown fields are rejected.

A successful toss result includes `command_sent`, `attempted_at`, `cycle_elapsed_ms`, `controller_protocol`, `controller_acknowledged`, `physical_completion_verified`, and separate `motor_command` and `lamp_off_command` reports. Each report includes the helper exit code, elapsed time, bounded stdout/stderr, and `uart_write_accepted`. The factory protocol is write-only, so a normal device result has `controller_acknowledged: false` and `physical_completion_verified: false`; it proves command delivery to the UART helper, not physical treat movement.

## Wi-Fi transaction

`PUT /network`:

```json
{"ssid":"your-network","passphrase":"your-network-password"}
```

The passphrase is 8–63 UTF-8 bytes, or a 64-digit hexadecimal WPA2 PSK. The response contains `transaction_id`, `requires_confirmation: true`, `expires_in: 300`, and `reboot_rolls_back: true`.

The manager allows the acknowledgment to leave before reconnecting. Poll `GET /network` after reconnecting, then:

```json
{"transaction_id":"the-returned-id"}
```

Send that body to `POST /network/confirm`. The active trial must be connected, unexpired, and from the current boot. A successful response means the confirmed profile has been synced to disk. The credentials are never echoed.

## Feed schedules

`PUT /schedules` replaces the entire array:

```json
[
  {
    "id":"breakfast",
    "time":"08:00",
    "days":[0,1,2,3,4,5,6],
    "enabled":true,
    "voice_id":null
  }
]
```

Monday is 0. There are at most 32 schedules. Each performs one toss, optionally preceded by a stored voice message. A schedule/date/local-minute ID is persisted before execution. Missed minutes are skipped. Distinct schedules within the motor cooldown can be deferred within their minute; no catch-up run occurs later.

## Uploads and notifications

`media.upload_url` receives an authenticated binary POST with Content-Type, Content-Length, X-Furbo-Media-Id and X-Furbo-Device. `upload_token_file` supplies an optional bearer token.

`media.webhook_url` receives JSON events. A signing secret of at least 32 bytes is required. Verify:

```text
X-Furbo-Signature = sha256=hex(HMAC-SHA256(secret, timestamp + "." + raw_body))
X-Furbo-Timestamp = current UNIX seconds at delivery
X-Furbo-Event-Id = stable event ID
```

Receivers should check timestamp freshness and deduplicate the stable event ID. Successful 2xx responses acknowledge delivery. Pending binary uploads and webhook events are stored in bounded queues and retried with backoff. MQTT notifications carry metadata and media IDs, not media bytes.


## Connection and event settings

GET /settings returns config plus secret-presence flags. PUT accepts any of timezone, http, mqtt, media, detection, camera, and watchdog; nested sections use the complete object returned by GET. Blank/unset MQTT passwords preserve the installed password. Credentials remain in the protected device JSON or dedicated mode-0600 secret files.

Optional write-only fields are server_certificate_pem with server_private_key_pem, mqtt_ca_pem, endpoint_ca_pem, upload_token, and webhook_secret. HTTPS certificate/key pairs and CA certificates are validated before applying. A callback signing secret must have at least 32 bytes.

Saving restarts the API/MQTT service after its acknowledgment. Listener changes return the new addresses. Existing accounts, keys, Wi-Fi and media remain intact.

media.retention_days = 0 disables age expiry; min_free_bytes reserves filesystem space. Cleanup checks every minute and before captures, protecting scheduled voice messages and pending uploads.

camera.overlay_text is blank to hide the logo, or a short ASCII label (24 bytes maximum); show_timestamp controls the native timestamp. No transcoding is used.

watchdog.audio_cpu_percent, rtsp_cpu_percent (1–100) and consecutive_readings (2–720) configure CPU-triggered recovery. Defaults are 50, 50 and 24; the factory comparison is adjusted so the configured count is exact. The legacy monitor must load the generated root-only threshold file as described in deployment.

The log is NDJSON, bounded to 256 KiB/1024 entries in RAM. It records `command_started` before execution, the completed action/result, controller diagnostics, and runtime errors, with structured secret fields redacted. This distinguishes a click that reached the API from a helper failure. Request bodies, passwords and private keys are not diagnostic payloads. It clears on process restart.


HTTP callback/upload URLs are accepted directly. allow_insecure_http is a legacy ignored input; no separate permission switch is needed. Media.allow_self_signed applies to uploads; webhook_allow_self_signed applies to event callbacks. MQTT has its own allow_self_signed flag.

Optional self-signed trust remembers the first certificate for each endpoint in the protected tls-pins.json file. Certificate names, time validity and handshake signatures remain checked. Unexpected private-certificate changes fail until an administrator sends reset_tls_pins:true through settings. Public/custom-CA trust remains available in Advanced TLS.

media.webhook_events is an optional array of event kinds. An empty array sends all events. This filters signed callback notifications; MQTT tele/EVENT still supplies the event stream.

UI language query parameters select only the initial language. Changing the dropdown removes that parameter; navigation does not carry it into page links.


http.http_enabled and http.https_enabled independently enable the two listeners. Both may be disabled for MQTT-only operation; the service and scheduled jobs continue running. A physical reset restores HTTP 0.0.0.0:8080 and HTTPS 0.0.0.0:8443 with a dedicated recovery certificate, while preserving MQTT/media configuration.

The default admin account cannot be disabled or demoted, even when other administrators exist. Its password can still be changed.
