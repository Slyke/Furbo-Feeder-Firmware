use crate::{
    app::App,
    config::Config,
    hardware::Request,
    model::{self, Action, Command, Role},
};
use chrono::{Datelike, Timelike};
use hmac::{Hmac, Mac};
use serde_json::json;
use sha2::Sha256;
use std::{
    sync::Arc,
    time::{Duration, Instant},
};

pub fn start(app: Arc<App>) {
    tokio::spawn(schedules(app.clone()));
    tokio::spawn(deliveries(app.clone()));
    tokio::spawn(retention(app.clone()));
    if app.cfg.detection.motion || app.cfg.detection.sound {
        tokio::spawn(detection(app));
    }
}
async fn retention(app: Arc<App>) {
    loop {
        if let Ok(_permit) = app.commands.try_acquire() {
            if let Err(error) = app.prune_media(0).await {
                crate::log_error!("retention: {error}");
            }
        }
        tokio::time::sleep(Duration::from_secs(60)).await;
    }
}
async fn schedules(app: Arc<App>) {
    loop {
        tokio::time::sleep(Duration::from_secs(5)).await;
        if !model::clock_valid() || app.setup_required().await.unwrap_or(true) {
            continue;
        }
        let Ok(now) = crate::timezone::at(&app.zone, chrono::Utc::now()) else {
            continue;
        };
        let time = format!("{:02}:{:02}", now.hour(), now.minute());
        let schedules = app.db.lock().await.schedules.clone();
        for schedule in schedules {
            if schedule.enabled
                && schedule.time == time
                && schedule
                    .days
                    .contains(&(now.weekday().num_days_from_monday() as u8))
            {
                let id = format!(
                    "sch_{}_{}_{}",
                    schedule.id,
                    now.format("%Y%m%d"),
                    now.format("%H%M")
                );
                if app.db.lock().await.seen.iter().any(|s| s.id == id) {
                    continue;
                }
                let cmd = Command {
                    id,
                    issued_at: model::now(),
                    action: Action::Toss {},
                };
                if let Err(e) = app.execute(cmd, Role::Operator, schedule.voice_id).await {
                    crate::log_error!("scheduled feed deferred: {e}");
                }
            }
        }
    }
}
async fn deliveries(app: Arc<App>) {
    let mut backoff = 2u64;
    loop {
        tokio::time::sleep(Duration::from_secs(backoff)).await;
        if !model::clock_valid() || app.setup_required().await.unwrap_or(true) {
            continue;
        }
        let (upload, event) = {
            let db = app.db.lock().await;
            (
                db.pending_uploads.first().cloned(),
                db.outbox.first().cloned(),
            )
        };
        let mut failed = false;
        if let Some(id) = upload {
            if let Ok(_permit) = app.commands.try_acquire() {
                let missing = app.media_path(&id).is_err();
                if missing || app.upload(&id).await.is_ok() {
                    let mut db = app.db.lock().await;
                    db.pending_uploads.retain(|u| u != &id);
                    if db.save(&app.cfg.data_dir).is_err() {
                        crate::log_error!("cannot persist upload acknowledgment");
                    }
                    drop(db);
                    app.event(
                        if missing {
                            "upload_media_missing"
                        } else {
                            "uploaded"
                        },
                        json!({"media_id":id}),
                    )
                    .await;
                } else {
                    failed = true;
                    crate::log_error!("upload failed; retrying with backoff");
                }
            }
        }
        if let Some(event) = event {
            let result = async {
                let secret = Config::secret(&app.cfg.media.webhook_secret_file)?
                    .ok_or_else(|| anyhow::anyhow!("webhook secret missing"))?;
                anyhow::ensure!(
                    secret.len() >= 32,
                    "webhook signing secret must be at least 32 bytes"
                );
                let bytes = serde_json::to_vec(&event)?;
                let timestamp = model::now().to_string();
                let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes())?;
                mac.update(timestamp.as_bytes());
                mac.update(b".");
                mac.update(&bytes);
                let hex = mac
                    .finalize()
                    .into_bytes()
                    .iter()
                    .map(|b| format!("{b:02x}"))
                    .collect::<String>();
                let response = app
                    .webhook_client
                    .post(
                        app.cfg
                            .media
                            .webhook_url
                            .as_ref()
                            .ok_or_else(|| anyhow::anyhow!("webhook endpoint missing"))?,
                    )
                    .header("Content-Type", "application/json")
                    .header("X-Furbo-Timestamp", timestamp)
                    .header("X-Furbo-Event-Id", event["id"].as_str().unwrap_or(""))
                    .header("X-Furbo-Signature", format!("sha256={hex}"))
                    .body(bytes)
                    .send()
                    .await?;
                anyhow::ensure!(response.status().is_success(), "webhook rejected");
                anyhow::Ok(())
            }
            .await;
            if result.is_ok() {
                let mut db = app.db.lock().await;
                db.outbox.retain(|v| v["id"] != event["id"]);
                if db.save(&app.cfg.data_dir).is_err() {
                    crate::log_error!("cannot persist webhook acknowledgment");
                }
            } else {
                failed = true;
                crate::log_error!("webhook failed; retrying with backoff");
            }
        }
        backoff = if failed { (backoff * 2).min(300) } else { 2 };
    }
}
async fn detection(app: Arc<App>) {
    let mut previous: Option<Vec<u8>> = None;
    let mut last_event = Instant::now() - Duration::from_secs(app.cfg.detection.cooldown_seconds);
    loop {
        tokio::time::sleep(Duration::from_secs(app.cfg.detection.interval_seconds)).await;
        if !model::clock_valid() || app.setup_required().await.unwrap_or(true) {
            continue;
        }
        let Ok(permit) = app.commands.try_acquire() else {
            continue;
        };
        let mut event = None;
        if app.cfg.detection.motion {
            if let Ok(r) = app.hw(Request::MotionSample, &[]).await {
                if let Some(before) = &previous {
                    if before.len() == r.bytes.len() && !before.is_empty() {
                        // Compare sparse luma samples. This is motion, not object recognition.
                        let changed = before
                            .iter()
                            .zip(&r.bytes)
                            .filter(|(a, b)| a.abs_diff(**b) > 25)
                            .count() as f64
                            / before.len() as f64;
                        if changed >= app.cfg.detection.motion_threshold {
                            event = Some(("motion", json!({"changed_fraction":changed})));
                        }
                    }
                }
                previous = Some(r.bytes);
            }
        }
        if app.cfg.detection.sound {
            if let Ok(r) = app.hw(Request::SoundSample, &[]).await {
                let rms = r.value["rms"].as_f64().unwrap_or(0.0);
                if rms >= app.cfg.detection.sound_threshold {
                    event = Some(("sound", json!({"rms":rms})));
                }
            }
        }
        drop(permit);
        if let Some((kind, details)) = event {
            if last_event.elapsed() >= Duration::from_secs(app.cfg.detection.cooldown_seconds) {
                last_event = Instant::now();
                app.event(kind, details).await;
                if app.cfg.detection.capture_on_event {
                    let cmd = Command {
                        id: format!("event_{}", crate::auth::random(12)),
                        issued_at: model::now(),
                        action: Action::Capture {
                            upload: app.cfg.media.auto_upload,
                        },
                    };
                    let _ = app.execute(cmd, Role::Operator, None).await;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        body::Bytes,
        extract::{OriginalUri, State},
        http::{HeaderMap, StatusCode},
        routing::post,
        Router,
    };
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tokio::sync::Mutex;

    #[derive(Default)]
    struct Receiver {
        uploads: AtomicUsize,
        events: AtomicUsize,
        requests: Mutex<Vec<(String, HeaderMap, Vec<u8>)>>,
    }

    async fn receive(
        State(state): State<Arc<Receiver>>,
        OriginalUri(uri): OriginalUri,
        headers: HeaderMap,
        body: Bytes,
    ) -> StatusCode {
        let counter = if uri.path() == "/upload" {
            &state.uploads
        } else {
            &state.events
        };
        let attempt = counter.fetch_add(1, Ordering::SeqCst);
        state
            .requests
            .lock()
            .await
            .push((uri.path().to_owned(), headers, body.to_vec()));
        if attempt == 0 {
            StatusCode::SERVICE_UNAVAILABLE
        } else {
            StatusCode::NO_CONTENT
        }
    }

    #[tokio::test]
    async fn durable_upload_and_signed_webhook_retry() {
        let receiver = Arc::new(Receiver::default());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let routes = Router::new()
            .route("/upload", post(receive))
            .route("/event", post(receive))
            .with_state(receiver.clone());
        let server = tokio::spawn(async move {
            axum::serve(listener, routes).await.unwrap();
        });
        let dir = std::env::temp_dir().join(format!("furbo-delivery-{}", crate::auth::random(9)));
        crate::store::private_dir(&dir).unwrap();
        crate::store::private_dir(&dir.join("media")).unwrap();
        let mut cfg: Config = serde_json::from_str(include_str!("../config.example.json")).unwrap();
        cfg.data_dir = dir.clone();
        cfg.http.https_listen = None;
        cfg.media.auto_upload = true;
        cfg.media.upload_url = Some(format!("http://{address}/upload"));
        cfg.media.webhook_url = Some(format!("http://{address}/event"));
        cfg.media.allow_insecure_http = true;
        cfg.media.upload_token_file = Some(dir.join("upload-token"));
        cfg.media.webhook_secret_file = Some(dir.join("webhook-secret"));
        let signing_key = "synthetic-test-webhook-signing-key-32";
        crate::store::atomic_write(&dir.join("upload-token"), b"synthetic-upload-token").unwrap();
        crate::store::atomic_write(&dir.join("webhook-secret"), signing_key.as_bytes()).unwrap();
        crate::store::atomic_write(&dir.join("jwt-secret"), crate::auth::random(48).as_bytes())
            .unwrap();
        let mut db = crate::store::Database::default();
        db.users.insert(
            "admin".into(),
            crate::auth::user("synthetic-test-password", Role::Admin).unwrap(),
        );
        db.save(&dir).unwrap();
        let app = App::open(cfg.clone()).unwrap();
        let result = app
            .execute(
                Command {
                    id: "synthetic-capture".into(),
                    issued_at: model::now(),
                    action: Action::Capture { upload: false },
                },
                Role::Operator,
                None,
            )
            .await
            .unwrap();
        assert_eq!(result["ok"], true);
        let media_id = result["result"]["id"].as_str().unwrap().to_owned();
        let expected = std::fs::read(app.media_path(&media_id).unwrap().0).unwrap();
        let initial_events = app.db.lock().await.outbox.len();
        assert!(initial_events > 0);
        drop(app);
        // Restart before delivery: both queues must survive in the database.
        let app = App::open(cfg).unwrap();
        assert_eq!(app.db.lock().await.pending_uploads, vec![media_id.clone()]);
        assert_eq!(app.db.lock().await.outbox.len(), initial_events);
        let worker = tokio::spawn(deliveries(app.clone()));
        tokio::time::timeout(Duration::from_secs(30), async {
            loop {
                tokio::time::sleep(Duration::from_millis(100)).await;
                let db = app.db.lock().await;
                if db.pending_uploads.is_empty() && db.outbox.is_empty() {
                    break;
                }
            }
        })
        .await
        .unwrap();
        worker.abort();
        let _ = worker.await;
        server.abort();
        let _ = server.await;
        let requests = receiver.requests.lock().await;
        let uploads = requests
            .iter()
            .filter(|(path, _, _)| path == "/upload")
            .collect::<Vec<_>>();
        assert_eq!(uploads.len(), 2);
        for (_, headers, body) in uploads {
            assert_eq!(headers["authorization"], "Bearer synthetic-upload-token");
            assert_eq!(headers["content-type"], "image/jpeg");
            assert_eq!(headers["x-furbo-media-id"], media_id);
            assert_eq!(headers["content-length"], expected.len().to_string());
            assert_eq!(body, &expected);
        }
        let events = requests
            .iter()
            .filter(|(path, _, _)| path == "/event")
            .collect::<Vec<_>>();
        assert!(events.len() > initial_events);
        assert_eq!(events[0].2, events[1].2);
        for (_, headers, body) in events {
            let timestamp = headers["x-furbo-timestamp"].to_str().unwrap();
            assert!(model::now().abs_diff(timestamp.parse::<i64>().unwrap()) < 30);
            let mut mac = Hmac::<Sha256>::new_from_slice(signing_key.as_bytes()).unwrap();
            mac.update(timestamp.as_bytes());
            mac.update(b".");
            mac.update(body);
            let expected_signature = mac
                .finalize()
                .into_bytes()
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<String>();
            assert_eq!(
                headers["x-furbo-signature"],
                format!("sha256={expected_signature}")
            );
            let event: serde_json::Value = serde_json::from_slice(body).unwrap();
            assert_eq!(headers["x-furbo-event-id"], event["id"].as_str().unwrap());
        }
        assert!(crate::store::Database::open(&dir)
            .unwrap()
            .pending_uploads
            .is_empty());
        assert!(crate::store::Database::open(&dir)
            .unwrap()
            .outbox
            .is_empty());
        drop(requests);
        drop(app);
        std::fs::remove_dir_all(dir).unwrap();
    }
}
