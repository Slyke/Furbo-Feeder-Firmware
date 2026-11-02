use crate::{
    config::Config,
    hardware::{self, Request},
    model::{self, Action, Command, Role},
    store::{self, Database, Seen},
};
use anyhow::{ensure, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    collections::{BTreeMap, VecDeque},
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};
use tokio::sync::{broadcast, Mutex, Notify, Semaphore};

pub struct HlsSegment {
    pub sequence: u64,
    pub duration: f64,
    pub bytes: Vec<u8>,
}

pub struct HlsState {
    pub init: Vec<u8>,
    pub segments: VecDeque<HlsSegment>,
    pub ended: bool,
    pub error: Option<String>,
}

pub struct HlsSession {
    pub created: Instant,
    pub state: Mutex<HlsState>,
    pub changed: Notify,
    pub cancelled: AtomicBool,
    pub cancel: Notify,
}

impl HlsSession {
    pub fn new(init: Vec<u8>) -> Self {
        Self {
            created: Instant::now(),
            state: Mutex::new(HlsState {
                init,
                segments: VecDeque::with_capacity(6),
                ended: false,
                error: None,
            }),
            changed: Notify::new(),
            cancelled: AtomicBool::new(false),
            cancel: Notify::new(),
        }
    }

    pub fn stop(&self) {
        self.cancelled.store(true, Ordering::Relaxed);
        self.cancel.notify_waiters();
    }
}

pub struct App {
    pub cfg: Config,
    pub config_path: Mutex<Option<PathBuf>>,
    pub zone: tz::TimeZone,
    pub db: Mutex<Database>,
    pub secret: Mutex<Vec<u8>>,
    pub commands: Semaphore,
    pub passwords: Semaphore,
    pub streams: Arc<Semaphore>,
    pub hls_sessions: Mutex<BTreeMap<String, Arc<HlsSession>>>,
    pub events: broadcast::Sender<Value>,
    pub recent: Mutex<VecDeque<Value>>,
    pub setup_tokens: Mutex<std::collections::BTreeMap<String, (Instant, String)>>,
    pub login_rate: Mutex<(Instant, u16)>,
    pub command_rate: Mutex<(Instant, u16)>,
    pub client: reqwest::Client,
    pub webhook_client: reqwest::Client,
    pub started: Instant,
    pub mqtt_connected: std::sync::atomic::AtomicBool,
}
#[derive(Clone, Serialize, Deserialize)]
pub struct MediaInfo {
    pub id: String,
    pub content_type: String,
    pub bytes: u64,
    pub created: i64,
}
impl App {
    pub fn open(cfg: Config) -> Result<Arc<Self>> {
        let secret = std::fs::read(cfg.data_dir.join("jwt-secret"))?;
        ensure!(secret.len() >= 32, "invalid JWT secret");
        let db = Database::open(&cfg.data_dir)?;
        if !cfg.simulate {
            let _: serde_json::Value = serde_json::from_slice(
                &std::fs::read("/run/furbo-local/auth-reset.json")
                    .context("hardware auth state unavailable")?,
            )?;
        }
        let client = crate::tls::http_client(
            &cfg,
            cfg.media.upload_url.as_deref(),
            cfg.media.allow_self_signed,
        )?;
        let webhook_client = crate::tls::http_client(
            &cfg,
            cfg.media.webhook_url.as_deref(),
            cfg.media.webhook_allow_self_signed,
        )?;
        let (events, _) = broadcast::channel(32);
        Ok(Arc::new(Self {
            zone: crate::timezone::load(&cfg.timezone)?,
            cfg,
            config_path: Mutex::new(None),
            db: Mutex::new(db),
            secret: Mutex::new(secret),
            commands: Semaphore::new(1),
            passwords: Semaphore::new(1),
            streams: Arc::new(Semaphore::new(2)),
            hls_sessions: Mutex::new(BTreeMap::new()),
            events,
            recent: Mutex::new(VecDeque::with_capacity(128)),
            setup_tokens: Mutex::new(std::collections::BTreeMap::new()),
            login_rate: Mutex::new((Instant::now(), 0)),
            command_rate: Mutex::new((Instant::now(), 0)),
            client,
            webhook_client,
            started: Instant::now(),
            mqtt_connected: std::sync::atomic::AtomicBool::new(false),
        }))
    }
    pub async fn setup_required(&self) -> Result<bool> {
        let empty = self.db.lock().await.users.is_empty();
        if self.cfg.simulate {
            return Ok(empty);
        }
        let v: Value = serde_json::from_slice(&std::fs::read("/run/furbo-local/auth-reset.json")?)?;
        Ok(empty || v["setup_required"] == true)
    }
    pub async fn hw(&self, r: Request, body: &[u8]) -> Result<hardware::Reply> {
        hardware::call(&self.cfg.hardware_socket, self.cfg.simulate, r, body).await
    }
    pub async fn event(&self, kind: &str, details: Value) {
        crate::logging::record("info", kind, details.clone());
        let e = json!({"id":crate::auth::random(12),"device":self.cfg.device_id,"timestamp":model::now(),"kind":kind,"details":details});
        {
            let mut r = self.recent.lock().await;
            if r.len() == 128 {
                r.pop_front();
            }
            r.push_back(e.clone());
        }
        if self.cfg.media.webhook_url.is_some()
            && (self.cfg.media.webhook_events.is_empty()
                || self
                    .cfg
                    .media
                    .webhook_events
                    .iter()
                    .any(|event| event == kind))
        {
            let mut db = self.db.lock().await;
            if db.outbox.len() < 32 {
                db.outbox.push(e.clone());
                if db.save(&self.cfg.data_dir).is_err() {
                    crate::log_error!("could not persist webhook event");
                }
            } else {
                crate::log_error!("webhook outbox full");
            }
        }
        let _ = self.events.send(e);
    }
    pub async fn execute(
        self: &Arc<Self>,
        cmd: Command,
        role: Role,
        voice: Option<String>,
    ) -> Result<Value> {
        ensure!(!self.setup_required().await?, "password setup required");
        ensure!(role.can_control(), "operator role required");
        if matches!(
            cmd.action,
            Action::WifiStage { .. }
                | Action::WifiConfirm { .. }
                | Action::WifiRollback {}
                | Action::Configure { .. }
        ) {
            ensure!(role.is_admin(), "administrator role required for Wi-Fi");
        }
        ensure!(model::valid_id(&cmd.id), "invalid command id");
        ensure!(
            model::clock_valid(),
            "clock unset; commands disabled until time is set"
        );
        ensure!(
            model::now().abs_diff(cmd.issued_at) <= self.cfg.mqtt.max_command_age_seconds as u64,
            "stale command"
        );
        let _permit = self
            .commands
            .try_acquire()
            .context("device busy; retry with the same command id")?;
        {
            let mut rate = self.command_rate.lock().await;
            if rate.0.elapsed() > Duration::from_secs(60) {
                *rate = (Instant::now(), 0);
            }
            ensure!(rate.1 < 64, "command rate limit: 64 per minute");
            rate.1 += 1;
        }
        {
            let mut db = self.db.lock().await;
            if let Some(previous) = db.seen.iter().find(|s| s.id == cmd.id) {
                return Ok(previous.result.clone());
            }
            if matches!(cmd.action, Action::Toss {}) {
                ensure!(
                    model::now() - db.last_feed >= 30,
                    "motor cooldown: 30 seconds"
                );
                db.last_feed = model::now();
            }
            // Reserve before side effects. A crash cannot make a retry toss twice.
            db.seen.push(Seen{id:cmd.id.clone(),timestamp:model::now(),result:json!({"id":cmd.id,"ok":false,"error":"reserved; execution incomplete or outcome unknown"})});
            if db.seen.len() > 1024 {
                db.seen.remove(0);
            }
            db.save(&self.cfg.data_dir)?;
        }
        let description = cmd.action.description();
        crate::logging::record(
            "info",
            "command_started",
            json!({"id":cmd.id.clone(),"action":description.clone()}),
        );
        let result = self.perform(&cmd.action, voice).await;
        let response = match result {
            Ok(v) => json!({"id":cmd.id,"ok":true,"result":v}),
            Err(e) => json!({"id":cmd.id,"ok":false,"error":e.to_string()}),
        };
        {
            let mut db = self.db.lock().await;
            if let Some(s) = db.seen.iter_mut().find(|s| s.id == cmd.id) {
                s.result = response.clone();
            }
            db.save(&self.cfg.data_dir)?;
        }
        self.event("command", json!({"action":description,"response":response}))
            .await;
        Ok(response)
    }
    async fn perform(self: &Arc<Self>, action: &Action, voice: Option<String>) -> Result<Value> {
        match action {
            Action::Configure { settings } => {
                crate::settings::update(self, serde_json::from_value(settings.clone())?).await
            }
            Action::SetSchedules { schedules } => {
                ensure!(schedules.len() <= 32, "schedule limit: 32");
                let mut ids = std::collections::BTreeSet::new();
                for s in schedules {
                    s.validate()?;
                    ensure!(
                        s.id.len() <= 32 && ids.insert(s.id.clone()),
                        "invalid or duplicate schedule id"
                    );
                    if let Some(id) = &s.voice_id {
                        ensure!(
                            self.media_path(id)?.1.content_type == "audio/wav",
                            "voice must be WAV"
                        );
                    }
                }
                let mut db = self.db.lock().await;
                db.schedules = schedules.clone();
                db.save(&self.cfg.data_dir)?;
                Ok(json!({"saved":schedules.len()}))
            }
            Action::DeleteMedia { media_id } => {
                let mut db = self.db.lock().await;
                ensure!(
                    !db.schedules
                        .iter()
                        .any(|s| s.voice_id.as_ref() == Some(media_id)),
                    "media is used by a feeding schedule"
                );
                let (path, _) = self.media_path(media_id)?;
                std::fs::remove_file(path)?;
                db.pending_uploads.retain(|id| id != media_id);
                db.save(&self.cfg.data_dir)?;
                Ok(json!({"deleted":media_id}))
            }
            Action::ExternalEvent { kind, details } => {
                ensure!(
                    ["motion", "sound", "bark", "person", "dog", "activity", "selfie"]
                        .contains(&kind.as_str()),
                    "unknown event kind"
                );
                ensure!(
                    serde_json::to_vec(details)?.len() <= 4096,
                    "event details too large"
                );
                self.event(kind, details.clone()).await;
                Ok(json!({"accepted":true}))
            }
            Action::WifiStage { ssid, passphrase } => Ok(self
                .hw(
                    Request::WifiConfigure {
                        ssid: ssid.clone(),
                        passphrase: passphrase.clone(),
                    },
                    &[],
                )
                .await?
                .value),
            Action::WifiConfirm { transaction_id } => Ok(self
                .hw(
                    Request::WifiConfirm {
                        transaction_id: transaction_id.clone(),
                    },
                    &[],
                )
                .await?
                .value),
            Action::WifiRollback {} => Ok(self.hw(Request::WifiRollback, &[]).await?.value),
            Action::Toss {} => {
                if let Some(id) = voice {
                    let (p, info) = self.media_path(&id)?;
                    ensure!(
                        info.content_type == "audio/wav",
                        "schedule voice must be WAV"
                    );
                    let bytes = std::fs::read(p)?;
                    self.hw(
                        Request::Play {
                            length: bytes.len(),
                        },
                        &bytes,
                    )
                    .await?;
                }
                let result = self.hw(Request::Toss, &[]).await?;
                Ok(result.value)
            }
            Action::Led { color } => Ok(self
                .hw(
                    Request::Led {
                        color: color.clone(),
                    },
                    &[],
                )
                .await?
                .value),
            Action::Tunnel { on } => Ok(self.hw(Request::Tunnel { on: *on }, &[]).await?.value),
            Action::Night { mode } => {
                let r = self.hw(Request::Night { mode: mode.clone() }, &[]).await?;
                let mut db = self.db.lock().await;
                db.night = mode.clone();
                db.save(&self.cfg.data_dir)?;
                Ok(r.value)
            }
            Action::Volume { percent } => {
                let r = self.hw(Request::Volume { percent: *percent }, &[]).await?;
                let mut db = self.db.lock().await;
                db.volume = *percent;
                db.save(&self.cfg.data_dir)?;
                Ok(r.value)
            }
            Action::Mute { muted } => {
                let r = self.hw(Request::Mute { muted: *muted }, &[]).await?;
                let mut db = self.db.lock().await;
                db.muted = *muted;
                db.save(&self.cfg.data_dir)?;
                Ok(r.value)
            }
            Action::Image {
                setting: crate::model::ImageSetting::DayNight,
                value,
            } => {
                ensure!((0..=1).contains(value), "day/night must be 0 or 1");
                let mode = if *value == 1 {
                    crate::model::NightMode::On
                } else {
                    crate::model::NightMode::Off
                };
                let result = self.hw(Request::Night { mode: mode.clone() }, &[]).await?;
                let mut db = self.db.lock().await;
                db.night = mode;
                db.save(&self.cfg.data_dir)?;
                Ok(result.value)
            }
            Action::Image { setting, value } => Ok(self
                .hw(
                    Request::Image {
                        setting: setting.clone(),
                        value: *value,
                    },
                    &[],
                )
                .await?
                .value),
            Action::Capture { upload } => {
                let r = self.hw(Request::Snapshot, &[]).await?;
                let info = self.save_media("jpg", &r.bytes).await?;
                if *upload || self.cfg.media.auto_upload {
                    self.enqueue_upload(&info.id).await?;
                }
                self.event("capture", json!(info)).await;
                Ok(json!(info))
            }
            Action::Record { seconds, upload } => {
                let r = self.hw(Request::Record { seconds: *seconds }, &[]).await?;
                let info = self.save_media("wav", &r.bytes).await?;
                if *upload || self.cfg.media.auto_upload {
                    self.enqueue_upload(&info.id).await?;
                }
                self.event("audio_recording", json!(info)).await;
                Ok(json!(info))
            }
            Action::Video { seconds, upload } => {
                ensure!(
                    (1..=30).contains(seconds),
                    "video duration must be 1..30 seconds"
                );
                ensure!(!self.cfg.simulate, "video needs real RTSP hardware");
                let _stream = self
                    .streams
                    .clone()
                    .try_acquire_owned()
                    .context("stream limit reached")?;
                let mut session = crate::rtsp::Session::open(true).await?;
                let mut bytes = session.init.clone();
                let start = Instant::now();
                let cap = (self.cfg.media.max_bytes / 2).min(4_194_304) as usize;
                while start.elapsed() < Duration::from_secs(*seconds as u64) {
                    let b = session.next().await?;
                    if bytes.len() + b.len() + 32768 > cap {
                        break;
                    }
                    bytes.extend(b);
                }
                ensure!(bytes.len() > 4096, "no usable video frames");
                let bytes = crate::mp4::finalize(bytes)?;
                let info = self.save_media("mp4", &bytes).await?;
                if *upload || self.cfg.media.auto_upload {
                    self.enqueue_upload(&info.id).await?;
                }
                self.event("video_recording", json!(info)).await;
                Ok(json!(info))
            }
            Action::Play { media_id } => {
                let (p, info) = self.media_path(media_id)?;
                ensure!(
                    info.content_type == "audio/wav",
                    "only WAV playback is supported"
                );
                let bytes = std::fs::read(p)?;
                Ok(self
                    .hw(
                        Request::Play {
                            length: bytes.len(),
                        },
                        &bytes,
                    )
                    .await?
                    .value)
            }
            Action::Upload { media_id } => {
                self.enqueue_upload(media_id).await?;
                Ok(json!({"queued":true}))
            }
        }
    }
    pub fn list_media(&self) -> Result<Vec<MediaInfo>> {
        let dir = crate::media_dir(&self.cfg);
        let mut result = vec![];
        for entry in std::fs::read_dir(dir)? {
            let e = entry?;
            let path = e.path();
            let meta = e.metadata()?;
            if !meta.is_file() || e.file_type()?.is_symlink() {
                continue;
            }
            let Some(ext) = path.extension().and_then(|s| s.to_str()) else {
                continue;
            };
            let ct = match ext {
                "jpg" => "image/jpeg",
                "wav" => "audio/wav",
                "mp4" => "video/mp4",
                _ => continue,
            };
            let Some(id) = path.file_stem().and_then(|s| s.to_str()) else {
                continue;
            };
            if !model::valid_id(id) {
                continue;
            }
            let created = meta
                .modified()?
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs() as i64;
            result.push(MediaInfo {
                id: id.to_owned(),
                content_type: ct.into(),
                bytes: meta.len(),
                created,
            });
        }
        result.sort_by_key(|i| i.created);
        Ok(result)
    }
    pub fn media_path(&self, id: &str) -> Result<(PathBuf, MediaInfo)> {
        ensure!(model::valid_id(id), "invalid media id");
        let info = self
            .list_media()?
            .into_iter()
            .find(|m| m.id == id)
            .context("media not found")?;
        let ext = match info.content_type.as_str() {
            "image/jpeg" => "jpg",
            "audio/wav" => "wav",
            _ => "mp4",
        };
        Ok((
            crate::media_dir(&self.cfg).join(format!("{id}.{ext}")),
            info,
        ))
    }
    pub async fn prune_media(&self, reserve: u64) -> Result<usize> {
        let protected = {
            let db = self.db.lock().await;
            db.schedules
                .iter()
                .filter_map(|s| s.voice_id.clone())
                .chain(db.pending_uploads.iter().cloned())
                .collect::<Vec<_>>()
        };
        let mut removed = 0;
        for item in self.list_media()? {
            let free = crate::stats::filesystem(&crate::media_dir(&self.cfg))["available_bytes"]
                .as_u64()
                .context("cannot check media filesystem free space")?;
            let expired = self.cfg.media.retention_days > 0
                && model::clock_valid()
                && model::now().saturating_sub(item.created)
                    > i64::from(self.cfg.media.retention_days) * 86400;
            if !protected.contains(&item.id)
                && (expired || free < self.cfg.media.min_free_bytes.saturating_add(reserve))
            {
                std::fs::remove_file(self.media_path(&item.id)?.0)?;
                removed += 1;
            }
        }
        let free = crate::stats::filesystem(&crate::media_dir(&self.cfg))["available_bytes"]
            .as_u64()
            .context("cannot check media filesystem free space")?;
        ensure!(
            free >= self.cfg.media.min_free_bytes.saturating_add(reserve),
            "media filesystem reserve exhausted"
        );
        if removed > 0 {
            self.event("retention", json!({"deleted_files":removed}))
                .await;
        }
        Ok(removed)
    }
    pub async fn save_media(&self, ext: &str, bytes: &[u8]) -> Result<MediaInfo> {
        if crate::media_dir(&self.cfg).starts_with("/dev/adc") {
            ensure!(
                std::fs::read_to_string("/proc/mounts")?
                    .lines()
                    .any(|line| line.split_whitespace().nth(1) == Some("/dev/adc")),
                "persistent media filesystem is not mounted"
            );
        }
        ensure!(
            bytes.len() as u64 <= self.cfg.media.max_bytes,
            "media exceeds quota"
        );
        self.prune_media(bytes.len() as u64).await?;
        let mut items = self.list_media()?;
        let protected = {
            let db = self.db.lock().await;
            db.schedules
                .iter()
                .filter_map(|s| s.voice_id.clone())
                .chain(db.pending_uploads.iter().cloned())
                .collect::<Vec<_>>()
        };
        let mut used = items.iter().map(|m| m.bytes).sum::<u64>();
        while used + bytes.len() as u64 > self.cfg.media.max_bytes
            || items.len() >= self.cfg.media.max_files
        {
            ensure!(!items.is_empty(), "media quota exhausted");
            let index = items
                .iter()
                .position(|m| !protected.contains(&m.id))
                .context("media quota is occupied by pending uploads or schedule voices")?;
            let first = items.remove(index);
            let (p, _) = self.media_path(&first.id)?;
            std::fs::remove_file(p)?;
            used -= first.bytes;
        }
        let id = format!("{}_{}", model::now(), crate::auth::random(9));
        let path = crate::media_dir(&self.cfg).join(format!("{id}.{ext}"));
        store::atomic_write(&path, bytes)?;
        Ok(self.media_path(&id)?.1)
    }
    pub async fn enqueue_upload(&self, id: &str) -> Result<()> {
        ensure!(
            self.cfg.media.upload_url.is_some(),
            "no upload endpoint configured"
        );
        self.media_path(id)?;
        let mut db = self.db.lock().await;
        if !db.pending_uploads.iter().any(|u| u == id) {
            ensure!(db.pending_uploads.len() < 32, "upload queue full");
            db.pending_uploads.push(id.to_owned());
            db.save(&self.cfg.data_dir)?;
        }
        Ok(())
    }
    pub async fn upload(&self, id: &str) -> Result<()> {
        ensure!(model::clock_valid(), "clock unset");
        let (path, info) = self.media_path(id)?;
        let file = tokio::fs::File::open(path).await?;
        let mut req = self
            .client
            .post(
                self.cfg
                    .media
                    .upload_url
                    .as_ref()
                    .context("upload endpoint missing")?,
            )
            .header("Content-Type", info.content_type)
            .header("Content-Length", info.bytes)
            .header("X-Furbo-Media-Id", id)
            .header("X-Furbo-Device", &self.cfg.device_id)
            .body(reqwest::Body::wrap_stream(
                tokio_util::io::ReaderStream::new(file),
            ));
        if let Some(token) = Config::secret(&self.cfg.media.upload_token_file)? {
            req = req.bearer_auth(token);
        }
        ensure!(
            req.send().await?.status().is_success(),
            "upload endpoint rejected media"
        );
        Ok(())
    }
    pub async fn status(&self) -> Value {
        let db = self.db.lock().await;
        json!({"mqtt":{"enabled":self.cfg.mqtt.enabled,"connected":self.mqtt_connected.load(std::sync::atomic::Ordering::Relaxed),"topic_prefix":self.cfg.mqtt.topic_prefix},"local_time":crate::timezone::at(&self.zone,chrono::Utc::now()).ok().map(|t|t.to_rfc3339()),"tzdb_version":crate::timezone::VERSION.trim(),"device":self.cfg.device_id,"simulated":self.cfg.simulate,"clock_valid":model::clock_valid(),"timestamp":model::now(),"timezone":self.cfg.timezone,"uptime_seconds":self.started.elapsed().as_secs(),"last_feed":db.last_feed,"volume":db.volume,"muted":db.muted,"night":db.night,"schedules":db.schedules.len(),"pending_uploads":db.pending_uploads.len(),"pending_webhooks":db.outbox.len(),"memory":std::fs::read_to_string("/proc/self/status").ok().and_then(|s|s.lines().find(|l|l.starts_with("VmRSS:")).map(str::to_owned)),"capabilities":{"video":"h264_aac_fmp4","snapshots":true,"voice":true,"toss":true,"lights":true,"night":true,"motion_detection":self.cfg.detection.motion,"sound_detection":self.cfg.detection.sound,"cloud_ai":false,"image_controls":if self.cfg.simulate || crate::hardware::oryx_image_available() {vec!["brightness","contrast","saturation","sharpness","hue","day_night","anti_flicker","white_balance"]} else {vec!["day_night","anti_flicker"]}}})
    }
}
pub async fn rate_limit(rate: &Mutex<(Instant, u16)>, limit: u16) -> Result<()> {
    let mut r = rate.lock().await;
    if r.0.elapsed() > Duration::from_secs(60) {
        *r = (Instant::now(), 0);
    }
    ensure!(r.1 < limit, "rate limit exceeded");
    r.1 += 1;
    Ok(())
}
