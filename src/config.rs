use anyhow::{ensure, Context, Result};
use serde::{Deserialize, Serialize};
use std::{
    net::SocketAddr,
    path::{Path, PathBuf},
};

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub device_id: String,
    pub serial_number: Option<String>,
    pub data_dir: PathBuf,
    pub hardware_socket: PathBuf,
    pub simulate: bool,
    pub timezone: String,
    pub http: Http,
    pub mqtt: Mqtt,
    pub media: Media,
    pub detection: Detection,
    #[serde(default)]
    pub camera: Camera,
    #[serde(default)]
    pub watchdog: crate::watchdog::Settings,
}
#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Camera {
    pub overlay_text: String,
    pub show_timestamp: bool,
}
impl Default for Camera {
    fn default() -> Self {
        Self {
            overlay_text: String::new(),
            show_timestamp: true,
        }
    }
}
#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Http {
    #[serde(default = "enabled_default")]
    pub http_enabled: bool,
    #[serde(default = "enabled_default")]
    pub https_enabled: bool,
    pub listen: SocketAddr,
    pub allow_insecure_lan: bool,
    pub https_listen: Option<SocketAddr>,
    pub certificate: Option<PathBuf>,
    pub private_key: Option<PathBuf>,
    pub max_connections: usize,
}
#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Mqtt {
    pub enabled: bool,
    pub host: String,
    pub port: u16,
    pub tls: bool,
    #[serde(default)]
    pub allow_self_signed: bool,
    pub ca_file: Option<PathBuf>,
    pub client_certificate: Option<PathBuf>,
    pub client_key: Option<PathBuf>,
    pub username: Option<String>,
    pub password_file: Option<PathBuf>,
    pub password: Option<String>,
    pub certificate_name: Option<String>,
    pub topic_prefix: String,
    pub max_command_age_seconds: i64,
}
#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Media {
    pub directory: Option<PathBuf>,
    pub max_bytes: u64,
    pub max_files: usize,
    #[serde(default)]
    pub retention_days: u16,
    #[serde(default = "default_free_space")]
    pub min_free_bytes: u64,
    pub auto_upload: bool,
    #[serde(default)]
    pub allow_self_signed: bool,
    #[serde(default)]
    pub webhook_events: Vec<String>,
    #[serde(default)]
    pub webhook_allow_self_signed: bool,
    pub upload_url: Option<String>,
    pub upload_token_file: Option<PathBuf>,
    pub webhook_url: Option<String>,
    pub webhook_secret_file: Option<PathBuf>,
    pub ca_file: Option<PathBuf>,
    #[serde(default, skip_serializing)]
    pub allow_insecure_http: bool,
}
#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Detection {
    pub motion: bool,
    pub sound: bool,
    pub interval_seconds: u64,
    pub motion_threshold: f64,
    pub sound_threshold: f64,
    pub cooldown_seconds: u64,
    pub capture_on_event: bool,
}
fn enabled_default() -> bool {
    true
}
fn default_free_space() -> u64 {
    8_388_608
}
impl Config {
    pub fn load(path: &Path) -> Result<Self> {
        ensure!(
            std::fs::metadata(path)?.len() <= 65536,
            "configuration exceeds 64 KiB"
        );
        let config: Self =
            serde_json::from_str(&std::fs::read_to_string(path).context("read config")?)?;
        config.validate()?;
        Ok(config)
    }
    pub fn validate(&self) -> Result<()> {
        self.watchdog.validate()?;
        ensure!(crate::model::valid_id(&self.device_id), "invalid device_id");
        ensure!(
            self.camera.overlay_text.len() <= 24
                && self
                    .camera
                    .overlay_text
                    .bytes()
                    .all(|b| b.is_ascii_graphic() || b == b' '),
            "overlay label must be at most 24 ASCII characters"
        );
        crate::timezone::load(&self.timezone).context("invalid timezone")?;
        ensure!(
            self.media.webhook_events.len() <= 32
                && self
                    .media
                    .webhook_events
                    .iter()
                    .all(|event| event.len() <= 40 && crate::model::valid_id(event)),
            "invalid callback event selection"
        );
        ensure!(
            !self.http.http_enabled
                || self.http.listen.ip().is_loopback()
                || self.http.allow_insecure_lan,
            "HTTP on the LAN requires allow_insecure_lan"
        );
        ensure!(
            (1..=32).contains(&self.http.max_connections),
            "max_connections must be 1..32"
        );
        if !self.simulate {
            ensure!(
                (!self.http.http_enabled || self.http.listen.port() >= 1024)
                    && self
                        .http
                        .https_listen
                        .is_none_or(|address| address.port() >= 1024),
                "listeners need unprivileged ports (1024 or higher)"
            );
        }
        ensure!(
            self.http
                .https_listen
                .is_none_or(|address| address.port() != self.http.listen.port()),
            "HTTP and HTTPS need different ports"
        );
        if self.http.https_enabled && self.http.https_listen.is_some() {
            ensure!(
                self.http.certificate.is_some() && self.http.private_key.is_some(),
                "HTTPS needs a certificate and key"
            );
        }
        ensure!(
            !self.mqtt.topic_prefix.trim_end_matches('/').is_empty()
                && self.mqtt.topic_prefix.len() < 100
                && !self.mqtt.topic_prefix.contains(['#', '+', '\0']),
            "invalid MQTT prefix"
        );
        ensure!(
            (1..=300).contains(&self.mqtt.max_command_age_seconds),
            "invalid command age limit"
        );
        ensure!(
            (1_048_576..=67_108_864).contains(&self.media.max_bytes),
            "media quota must be 1..64 MiB"
        );
        ensure!(
            self.media.retention_days <= 3650,
            "retention must be 0..3650 days"
        );
        ensure!(
            self.media.min_free_bytes <= 67_108_864,
            "free space reserve must be at most 64 MiB"
        );
        ensure!(
            (1..=128).contains(&self.media.max_files),
            "invalid media file limit"
        );
        ensure!(
            (5..=86400).contains(&self.detection.cooldown_seconds),
            "detection cooldown must be 5..86400 seconds"
        );
        ensure!(
            (5..=3600).contains(&self.detection.interval_seconds),
            "invalid detection interval"
        );
        ensure!(
            (0.0..=1.0).contains(&self.detection.motion_threshold)
                && (0.0..=1.0).contains(&self.detection.sound_threshold),
            "invalid detection threshold"
        );
        ensure!(
            self.mqtt.password.is_none() || self.mqtt.password_file.is_none(),
            "choose password or password_file"
        );
        ensure!(
            self.mqtt.client_certificate.is_some() == self.mqtt.client_key.is_some(),
            "mTLS needs both certificate and key"
        );
        for value in [&self.media.upload_url, &self.media.webhook_url]
            .into_iter()
            .flatten()
        {
            let u = reqwest::Url::parse(value)?;
            ensure!(
                u.scheme() == "https" || u.scheme() == "http",
                "endpoint must use HTTP or HTTPS"
            );
            ensure!(
                u.host_str().is_some()
                    && u.username().is_empty()
                    && u.password().is_none()
                    && u.fragment().is_none(),
                "invalid endpoint URL"
            );
        }
        ensure!(
            self.media.webhook_url.is_none() || self.media.webhook_secret_file.is_some(),
            "webhooks require a signing secret"
        );
        Ok(())
    }
    pub fn secret(path: &Option<PathBuf>) -> Result<Option<String>> {
        path.as_ref()
            .map(|p| {
                std::fs::read_to_string(p)
                    .map(|s| s.trim_end().to_owned())
                    .context("read credential file")
            })
            .transpose()
    }
}
