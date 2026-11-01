//! Administrator configuration updates. Secrets are write-only in the API.
use crate::{
    app::App,
    config::{Camera, Config, Detection, Http, Media, Mqtt},
    hardware::Request,
};
use anyhow::{ensure, Context, Result};
use serde::Deserialize;
use serde_json::{json, Value};
use std::sync::Arc;
#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct Update {
    pub timezone: Option<String>,
    pub http: Option<Http>,
    pub mqtt: Option<Mqtt>,
    pub media: Option<Media>,
    pub detection: Option<Detection>,
    pub camera: Option<Camera>,
    pub watchdog: Option<crate::watchdog::Settings>,
    #[serde(default)]
    pub clear_mqtt_password: bool,
    #[serde(default)]
    pub reset_tls_pins: bool,
    pub server_certificate_pem: Option<String>,
    pub server_private_key_pem: Option<String>,
    pub mqtt_ca_pem: Option<String>,
    pub endpoint_ca_pem: Option<String>,
    pub upload_token: Option<String>,
    pub webhook_secret: Option<String>,
}
pub fn view(cfg: &Config) -> Value {
    let mut config = serde_json::to_value(cfg).expect("config JSON");
    config["mqtt"]["password"] = Value::Null;
    json!({"config":config,"secrets":{
        "mqtt_password":cfg.mqtt.password.is_some()||cfg.mqtt.password_file.is_some(),
        "upload_token":cfg.media.upload_token_file.is_some(),
        "webhook_secret":cfg.media.webhook_secret_file.is_some()
    },"apply_restarts_service":true})
}
fn check_ca(pem: &str) -> Result<()> {
    let certs = rustls_pemfile::certs(&mut pem.as_bytes()).collect::<std::io::Result<Vec<_>>>()?;
    ensure!(!certs.is_empty(), "CA PEM contains no certificates");
    let mut roots = rustls::RootCertStore::empty();
    for cert in certs {
        roots.add(cert)?;
    }
    Ok(())
}
pub async fn update(app: &Arc<App>, patch: Update) -> Result<Value> {
    let mut config = app.cfg.clone();
    if let Some(v) = patch.timezone {
        config.timezone = v;
    }
    if let Some(v) = patch.http {
        config.http = v;
    }
    if let Some(mut v) = patch.mqtt {
        if patch.clear_mqtt_password {
            v.password = None;
            v.password_file = None;
        } else if v.password.is_none() && v.password_file.is_none() {
            v.password = config.mqtt.password.clone();
            v.password_file = config.mqtt.password_file.clone();
        }
        if v.password.is_some() {
            v.password_file = None;
        }
        config.mqtt = v;
    }
    if let Some(v) = patch.media {
        config.media = v;
    }
    if let Some(v) = patch.watchdog {
        config.watchdog = v;
    }
    if let Some(v) = patch.camera {
        config.camera = v;
    }
    if let Some(v) = patch.detection {
        config.detection = v;
    }
    ensure!(
        config.media.directory == app.cfg.media.directory,
        "media location cannot change through this endpoint"
    );
    let mut files = vec![];
    match (patch.server_certificate_pem, patch.server_private_key_pem) {
        (Some(cert), Some(key)) => {
            axum_server::tls_rustls::RustlsConfig::from_pem(
                cert.as_bytes().to_vec(),
                key.as_bytes().to_vec(),
            )
            .await?;
            let cert_path = config.data_dir.join("settings-server.crt");
            let key_path = config.data_dir.join("settings-server.key");
            files.push((cert_path.clone(), cert));
            files.push((key_path.clone(), key));
            config.http.certificate = Some(cert_path);
            config.http.private_key = Some(key_path);
        }
        (None, None) => {}
        _ => anyhow::bail!("upload both the HTTPS certificate and its matching private key"),
    }
    for (value, name, target) in [
        (
            patch.mqtt_ca_pem,
            "settings-mqtt-ca.pem",
            &mut config.mqtt.ca_file,
        ),
        (
            patch.endpoint_ca_pem,
            "settings-endpoint-ca.pem",
            &mut config.media.ca_file,
        ),
    ] {
        if let Some(pem) = value {
            if pem.is_empty() {
                *target = None;
            } else {
                check_ca(&pem)?;
                let path = config.data_dir.join(name);
                *target = Some(path.clone());
                files.push((path, pem));
            }
        }
    }
    for (value, name, target, min) in [
        (
            patch.upload_token,
            "settings-upload-token",
            &mut config.media.upload_token_file,
            1,
        ),
        (
            patch.webhook_secret,
            "settings-webhook-secret",
            &mut config.media.webhook_secret_file,
            32,
        ),
    ] {
        if let Some(secret) = value {
            if secret.is_empty() {
                *target = None;
            } else {
                ensure!(
                    secret.len() >= min && secret.len() <= 4096,
                    "invalid integration secret length"
                );
                let path = config.data_dir.join(name);
                *target = Some(path.clone());
                files.push((path, secret));
            }
        }
    }
    config.validate()?;
    if patch.reset_tls_pins {
        crate::tls::forget(&config.data_dir)?;
    }
    for (path, body) in files {
        crate::store::atomic_write(&path, body.as_bytes())?;
    }
    let urls = json!({"http":config.http.http_enabled.then(||config.http.listen.to_string()),"https":config.http.https_listen.filter(|_|config.http.https_enabled).map(|v|v.to_string())});
    if app.cfg.simulate {
        let path = app
            .config_path
            .lock()
            .await
            .clone()
            .context("configuration path unavailable")?;
        crate::store::atomic_write(&path, &serde_json::to_vec_pretty(&config)?)?;
    } else {
        app.hw(
            Request::UpdateConfig {
                config: Box::new(config),
            },
            &[],
        )
        .await?;
    }
    crate::logging::record("info", "settings_saved", json!({"restart_required":true}));
    Ok(json!({"ok":true,"restart_required":true,"listeners":urls}))
}
