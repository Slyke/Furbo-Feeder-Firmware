use anyhow::Context;
use axum::{
    body::{Body, Bytes},
    extract::{ConnectInfo, DefaultBodyLimit, Path, Query, Request, State},
    http::{header, HeaderValue, StatusCode},
    middleware::{self, Next},
    response::{Html, IntoResponse, Response},
    routing::{get, post, put},
    Extension, Json, Router,
};
macro_rules! ensure { ($cond:expr,$($arg:tt)*) => { if !$cond {return Err(Error(StatusCode::BAD_REQUEST,format!($($arg)*)));} }; }
use crate::{
    app::{self, App},
    auth::{self, Principal},
    hardware::Request as Hardware,
    model::{Command, Role, Schedule},
};
use serde::Deserialize;
use serde_json::{json, Value};
use std::{collections::BTreeSet, sync::Arc, time::Duration};
pub struct Error(StatusCode, String);
impl<E: Into<anyhow::Error>> From<E> for Error {
    fn from(e: E) -> Self {
        Self(StatusCode::BAD_REQUEST, e.into().to_string())
    }
}
impl IntoResponse for Error {
    fn into_response(self) -> Response {
        (self.0, Json(json!({"error":self.1}))).into_response()
    }
}
type Result<T> = std::result::Result<T, Error>;
fn denied() -> Error {
    Error(StatusCode::FORBIDDEN, "insufficient permission".into())
}
fn operator(p: &Principal) -> Result<()> {
    if p.role.can_control() {
        Ok(())
    } else {
        Err(denied())
    }
}
fn admin(p: &Principal) -> Result<()> {
    if p.role.is_admin() {
        Ok(())
    } else {
        Err(denied())
    }
}
async fn index() -> Html<&'static str> {
    Html(include_str!("../web/index.html"))
}
pub fn router(app: Arc<App>) -> Router {
    let protected = Router::new()
        .route("/me", get(me))
        .route("/time", post(set_time))
        .route("/network", get(network_status).put(network_configure))
        .route("/network/recover", post(network_recover))
        .route("/network/confirm", post(network_confirm))
        .route("/network/rollback", post(network_rollback))
        .route("/logout", post(logout))
        .route("/session/upgrade", post(upgrade_session))
        .route("/status", get(status))
        .route("/stats", get(stats))
        .route("/logs.ndjson", get(logs))
        .route(
            "/settings",
            get(settings)
                .put(save_settings)
                .layer(DefaultBodyLimit::max(32768)),
        )
        .route("/sensors", get(sensors))
        .route("/events", get(events).post(event))
        .route("/commands", post(command))
        .route("/schedules", get(schedules).put(set_schedules))
        .route("/media", get(media))
        .route("/media/{id}", get(download).delete(delete_media))
        .route("/voice", put(voice).layer(DefaultBodyLimit::max(1_536_000)))
        .route("/listen", post(listen))
        .route("/stream", get(stream))
        .route("/users", get(users).post(add_user))
        .route("/users/{username}", put(update_user))
        .route("/keys", get(keys).post(add_key))
        .route("/keys/{id}", axum::routing::delete(delete_key))
        .route_layer(middleware::from_fn_with_state(app.clone(), authenticate));
    let limit = app.cfg.http.max_connections;
    Router::new()
        .route("/", get(index))
        .route("/overview", get(index))
        .route("/recordings", get(index))
        .route("/feed-times", get(index))
        .route("/settings", get(index))
        .route("/connections", get(index))
        .route("/accounts", get(index))
        .route(
            "/app.js",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "application/javascript")],
                    include_str!("../web/app.js"),
                )
            }),
        )
        .route(
            "/style.css",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "text/css")],
                    include_str!("../web/style.css"),
                )
            }),
        )
        .route(
            "/i18n.js",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "application/javascript")],
                    include_str!("../web/i18n.js"),
                )
            }),
        )
        .route(
            "/locales/en.json",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "application/json")],
                    include_str!("../web/locales/en.json"),
                )
            }),
        )
        .route(
            "/locales/zh-CN.json",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "application/json")],
                    include_str!("../web/locales/zh-CN.json"),
                )
            }),
        )
        .route("/healthz", get(|| async { Json(json!({"ok":true})) }))
        .route(
            "/fonts/furbo-ui-sc.woff2",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "font/woff2")],
                    include_bytes!("../web/fonts/furbo-ui-sc.woff2").as_slice(),
                )
            }),
        )
        .route("/favicon.ico", get(|| async { StatusCode::NO_CONTENT }))
        .route("/api/v1/login", post(login))
        .route("/api/v1/setup/status", get(setup_status))
        .route("/api/v1/setup/password", post(setup_password))
        .nest("/api/v1", protected)
        .layer(DefaultBodyLimit::max(8192))
        .layer(tower::limit::ConcurrencyLimitLayer::new(limit))
        .layer(middleware::from_fn(headers))
        .with_state(app)
}
async fn headers(request: Request, next: Next) -> Response {
    let method = request.method().to_string();
    let route = request
        .extensions()
        .get::<axum::extract::MatchedPath>()
        .map(|v| v.as_str().to_owned())
        .unwrap_or_else(|| "unmatched".into());
    let mut r = next.run(request).await;
    if r.status().is_client_error() || r.status().is_server_error() || method != "GET" {
        crate::logging::record(
            if r.status().is_success() {
                "info"
            } else {
                "warn"
            },
            "http",
            json!({"method":method,"route":route,"status":r.status().as_u16()}),
        );
    }
    let h = r.headers_mut();
    h.insert(
        "X-Content-Type-Options",
        HeaderValue::from_static("nosniff"),
    );
    h.insert("X-Frame-Options", HeaderValue::from_static("DENY"));
    h.insert("Referrer-Policy", HeaderValue::from_static("no-referrer"));
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    h.insert("Content-Security-Policy",HeaderValue::from_static("default-src 'self'; script-src 'self'; style-src 'self'; img-src 'self' blob:; media-src 'self' blob:; connect-src 'self'; object-src 'none'; frame-ancestors 'none'; base-uri 'none'"));
    r
}
async fn authenticate(State(app): State<Arc<App>>, mut request: Request, next: Next) -> Response {
    if app.setup_required().await.unwrap_or(true) {
        return (
            StatusCode::LOCKED,
            Json(json!({"error":"password setup required","setup_required":true})),
        )
            .into_response();
    }
    let auth_header = request
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.strip_prefix("Bearer "));
    let key = request
        .headers()
        .get("X-API-Key")
        .and_then(|v| v.to_str().ok());
    let token = match (auth_header, key) {
        (Some(t), None) | (None, Some(t)) => t,
        _ => {
            return (
                StatusCode::UNAUTHORIZED,
                Json(json!({"error":"Bearer token or X-API-Key required"})),
            )
                .into_response()
        }
    };
    let principal = if token.starts_with("setup_") {
        let path = request.uri().path();
        if ![
            "/network",
            "/network/recover",
            "/network/confirm",
            "/network/rollback",
            "/time",
            "/me",
            "/status",
            "/logout",
            "/session/upgrade",
            "/stats",
        ]
        .iter()
        .any(|p| path.ends_with(p))
        {
            return (
                StatusCode::FORBIDDEN,
                Json(json!({"error":"set the clock then log in again"})),
            )
                .into_response();
        }
        let entry = app.setup_tokens.lock().await.get(token).cloned();
        if let Some((created, username)) = entry {
            let db = app.db.lock().await;
            match db.users.get(&username) {
                Some(u) if !u.disabled && created.elapsed() < Duration::from_secs(300) => {
                    Ok(Principal {
                        username,
                        role: u.role,
                    })
                }
                _ => Err(anyhow::anyhow!("expired setup session")),
            }
        } else {
            Err(anyhow::anyhow!("invalid setup session"))
        }
    } else {
        let db = app.db.lock().await;
        auth::verify(token, &db, &app.secret.lock().await, &app.cfg.device_id)
    };
    match principal {
        Ok(p) => {
            request.extensions_mut().insert(p);
            next.run(request).await
        }
        Err(_) => (
            StatusCode::UNAUTHORIZED,
            Json(json!({"error":"invalid or expired credentials"})),
        )
            .into_response(),
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Login {
    username: String,
    password: String,
}
async fn login(State(app): State<Arc<App>>, Json(input): Json<Login>) -> Result<Json<Value>> {
    ensure!(!app.setup_required().await?, "password setup required");
    app::rate_limit(&app.login_rate, 6).await.map_err(|_| {
        Error(
            StatusCode::TOO_MANY_REQUESTS,
            "login rate limit: 6 per minute".into(),
        )
    })?;
    ensure!(
        input.username.len() <= 40 && input.password.len() <= 128,
        "invalid credentials"
    );
    let permit = app
        .passwords
        .try_acquire()
        .map_err(|_| Error(StatusCode::TOO_MANY_REQUESTS, "login busy".into()))?;
    let (found, user) = {
        let db = app.db.lock().await;
        (
            db.users.get(&input.username).cloned(),
            db.users.values().next().cloned().context("no accounts")?,
        )
    };
    let candidate = found.clone().unwrap_or(user);
    let ok =
        tokio::task::spawn_blocking(move || auth::password_ok(&candidate, &input.password)).await?;
    drop(permit);
    if !ok || found.is_none() {
        return Err(Error(
            StatusCode::UNAUTHORIZED,
            "invalid credentials".into(),
        ));
    }
    if !crate::model::clock_valid() {
        let token = format!("setup_{}", auth::random(32));
        let mut tokens = app.setup_tokens.lock().await;
        tokens.retain(|_, (created, _)| created.elapsed() < Duration::from_secs(300));
        ensure!(tokens.len() < 8, "setup session limit");
        tokens.insert(token.clone(), (std::time::Instant::now(), input.username));
        return Ok(Json(
            json!({"token":token,"expires_in":300,"setup_only":true,"token_type":"Bearer"}),
        ));
    }
    let token = auth::issue_jwt(
        &input.username,
        &found.unwrap(),
        &app.secret.lock().await,
        &app.cfg.device_id,
    )?;
    Ok(Json(
        json!({"token":token,"expires_in":auth::SESSION_SECONDS,"token_type":"Bearer"}),
    ))
}
async fn me(Extension(p): Extension<Principal>) -> Json<Principal> {
    Json(p)
}
async fn logout(
    State(app): State<Arc<App>>,
    Extension(p): Extension<Principal>,
) -> Result<Json<Value>> {
    let mut db = app.db.lock().await;
    db.users
        .get_mut(&p.username)
        .context("unknown account")?
        .version += 1;
    db.save(&app.cfg.data_dir)?;
    Ok(Json(json!({"ok":true})))
}
async fn status(State(app): State<Arc<App>>) -> Json<Value> {
    Json(app.status().await)
}
async fn sensors(State(app): State<Arc<App>>) -> Result<Json<Value>> {
    let _permit = app.commands.try_acquire().context("device busy")?;
    Ok(Json(app.hw(Hardware::Lux, &[]).await?.value))
}
async fn command(
    State(app): State<Arc<App>>,
    Extension(p): Extension<Principal>,
    Json(cmd): Json<Command>,
) -> Result<Json<Value>> {
    operator(&p)?;
    Ok(Json(app.execute(cmd, p.role, None).await?))
}
async fn events(State(app): State<Arc<App>>) -> Json<Value> {
    Json(json!(app.recent.lock().await.iter().collect::<Vec<_>>()))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EventInput {
    kind: String,
    details: Value,
}
async fn event(
    State(app): State<Arc<App>>,
    Extension(p): Extension<Principal>,
    Json(input): Json<EventInput>,
) -> Result<Json<Value>> {
    operator(&p)?;
    ensure!(
        ["motion", "sound", "bark", "person", "dog", "activity", "selfie"]
            .contains(&input.kind.as_str()),
        "unknown event kind"
    );
    app::rate_limit(&app.command_rate, 64).await?;
    app.event(&input.kind, input.details).await;
    Ok(Json(json!({"ok":true})))
}
async fn schedules(State(app): State<Arc<App>>) -> Json<Value> {
    Json(json!(app.db.lock().await.schedules))
}
async fn set_schedules(
    State(app): State<Arc<App>>,
    Extension(p): Extension<Principal>,
    Json(schedules): Json<Vec<Schedule>>,
) -> Result<Json<Value>> {
    operator(&p)?;
    ensure!(schedules.len() <= 32, "schedule limit: 32");
    let _permit = app.commands.try_acquire().context("device busy")?;
    let mut ids = BTreeSet::new();
    for s in &schedules {
        s.validate()?;
        ensure!(
            s.id.len() <= 32 && ids.insert(s.id.clone()),
            "invalid or duplicate schedule id"
        );
        if let Some(id) = &s.voice_id {
            ensure!(
                app.media_path(id)?.1.content_type == "audio/wav",
                "voice must be WAV"
            );
        }
    }
    let mut db = app.db.lock().await;
    db.schedules = schedules;
    db.save(&app.cfg.data_dir)?;
    Ok(Json(json!({"ok":true})))
}
async fn settings(
    State(app): State<Arc<App>>,
    Extension(p): Extension<Principal>,
) -> Result<Json<Value>> {
    admin(&p)?;
    Ok(Json(crate::settings::view(&app.cfg)))
}
async fn save_settings(
    State(app): State<Arc<App>>,
    Extension(p): Extension<Principal>,
    Json(input): Json<crate::settings::Update>,
) -> Result<Json<Value>> {
    admin(&p)?;
    let _permit = app.commands.try_acquire().context("device busy")?;
    Ok(Json(crate::settings::update(&app, input).await?))
}
async fn logs(Extension(p): Extension<Principal>) -> Result<Response> {
    admin(&p)?;
    Ok((
        [
            (header::CONTENT_TYPE, "application/x-ndjson"),
            (
                header::CONTENT_DISPOSITION,
                "attachment; filename=furbo-log.ndjson",
            ),
        ],
        crate::logging::download(),
    )
        .into_response())
}
async fn media(State(app): State<Arc<App>>) -> Result<Json<Value>> {
    Ok(Json(json!(app.list_media()?)))
}
async fn download(State(app): State<Arc<App>>, Path(id): Path<String>) -> Result<Response> {
    let (path, info) = app.media_path(&id)?;
    if info.content_type == "video/mp4" {
        let bytes = crate::mp4::finalize(tokio::fs::read(path).await?)?;
        return Ok(([(header::CONTENT_TYPE, "video/mp4")], bytes).into_response());
    }
    let file = tokio::fs::File::open(path).await?;
    let body = Body::from_stream(tokio_util::io::ReaderStream::new(file));
    Ok((
        [
            (header::CONTENT_TYPE, info.content_type),
            (header::CONTENT_LENGTH, info.bytes.to_string()),
        ],
        body,
    )
        .into_response())
}
async fn delete_media(
    State(app): State<Arc<App>>,
    Extension(p): Extension<Principal>,
    Path(id): Path<String>,
) -> Result<Json<Value>> {
    operator(&p)?;
    let _permit = app.commands.try_acquire().context("device busy")?;
    let mut db = app.db.lock().await;
    ensure!(
        !db.schedules
            .iter()
            .any(|s| s.voice_id.as_ref() == Some(&id)),
        "media is used by a feeding schedule"
    );
    let (path, _) = app.media_path(&id)?;
    std::fs::remove_file(path)?;
    db.pending_uploads.retain(|u| u != &id);
    db.save(&app.cfg.data_dir)?;
    Ok(Json(json!({"ok":true})))
}
async fn voice(
    State(app): State<Arc<App>>,
    Extension(p): Extension<Principal>,
    body: Bytes,
) -> Result<Json<Value>> {
    operator(&p)?;
    let _permit = app.commands.try_acquire().context("device busy")?;
    crate::hardware::validate_wav(&body)?;
    let info = app.save_media("wav", &body).await?;
    Ok(Json(json!(info)))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Listen {
    seconds: u8,
}
async fn listen(
    State(app): State<Arc<App>>,
    Extension(p): Extension<Principal>,
    Json(input): Json<Listen>,
) -> Result<Response> {
    operator(&p)?;
    ensure!(
        (1..=5).contains(&input.seconds),
        "listening chunk must be 1..5 seconds"
    );
    let _permit = app.commands.try_acquire().context("device busy")?;
    let r = app
        .hw(
            Hardware::Record {
                seconds: input.seconds,
            },
            &[],
        )
        .await?;
    Ok(([(header::CONTENT_TYPE, "audio/wav")], r.bytes).into_response())
}
#[derive(Deserialize)]
struct StreamOptions {
    #[serde(default)]
    audio: bool,
}
async fn stream(
    State(app): State<Arc<App>>,
    Query(options): Query<StreamOptions>,
) -> Result<Response> {
    ensure!(
        !app.cfg.simulate,
        "RTSP streaming is unavailable in simulation"
    );
    let permit = app
        .streams
        .clone()
        .try_acquire_owned()
        .context("two streams already active")?;
    let mut session = crate::rtsp::Session::open(options.audio).await?;
    let mime = session.mime.clone();
    let body = async_stream::stream! {
        let _permit=permit;
        yield Ok::<Bytes,std::io::Error>(Bytes::from(session.init.clone()));
        let start=std::time::Instant::now();
        while start.elapsed()<Duration::from_secs(300){match session.next().await {Ok(b)=>yield Ok(Bytes::from(b)),Err(e)=>{yield Err(std::io::Error::other(e));break;}}}
    };
    let body: Body = Body::from_stream(body);
    Ok(([(header::CONTENT_TYPE, mime)], body).into_response())
}
async fn users(
    State(app): State<Arc<App>>,
    Extension(p): Extension<Principal>,
) -> Result<Json<Value>> {
    admin(&p)?;
    let db = app.db.lock().await;
    Ok(Json(json!(db
        .users
        .iter()
        .map(|(name, u)| json!({"username":name,"role":u.role,"disabled":u.disabled}))
        .collect::<Vec<_>>())))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct UserInput {
    username: String,
    password: String,
    role: Role,
}
async fn add_user(
    State(app): State<Arc<App>>,
    Extension(p): Extension<Principal>,
    Json(input): Json<UserInput>,
) -> Result<Json<Value>> {
    admin(&p)?;
    auth::validate_username(&input.username)?;
    let _permit = app
        .passwords
        .try_acquire()
        .context("password hashing busy")?;
    let u = tokio::task::spawn_blocking(move || auth::user(&input.password, input.role)).await??;
    let mut db = app.db.lock().await;
    ensure!(
        db.users.len() < 16 && !db.users.contains_key(&input.username),
        "account exists or account limit reached"
    );
    db.users.insert(input.username, u);
    db.save(&app.cfg.data_dir)?;
    Ok(Json(json!({"ok":true})))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct UserUpdate {
    password: Option<String>,
    role: Option<Role>,
    disabled: Option<bool>,
}
async fn update_user(
    State(app): State<Arc<App>>,
    Extension(p): Extension<Principal>,
    Path(username): Path<String>,
    Json(input): Json<UserUpdate>,
) -> Result<Json<Value>> {
    admin(&p)?;
    ensure!(
        username != "admin" || input.disabled != Some(true),
        "the default administrator cannot be disabled"
    );
    ensure!(
        username != "admin" || input.role.is_none_or(|role| role == Role::Admin),
        "the default administrator must retain administrator access"
    );

    let _permit = app
        .passwords
        .try_acquire()
        .context("password hashing busy")?;
    let hash = if let Some(pass) = input.password {
        Some(
            tokio::task::spawn_blocking(move || auth::user(&pass, Role::Viewer))
                .await??
                .password_hash,
        )
    } else {
        None
    };
    let mut db = app.db.lock().await;
    let mut user = db
        .users
        .get(&username)
        .context("account not found")?
        .clone();
    if let Some(h) = hash {
        user.password_hash = h;
    }
    if let Some(role) = input.role {
        user.role = role;
    }
    if let Some(disabled) = input.disabled {
        user.disabled = disabled;
    }
    user.version += 1;
    ensure!(
        user.role == Role::Admin && !user.disabled
            || db
                .users
                .iter()
                .any(|(n, u)| n != &username && u.role == Role::Admin && !u.disabled),
        "cannot remove the last active administrator"
    );
    db.users.insert(username, user);
    db.save(&app.cfg.data_dir)?;
    Ok(Json(json!({"ok":true})))
}
async fn keys(
    State(app): State<Arc<App>>,
    Extension(p): Extension<Principal>,
) -> Result<Json<Value>> {
    admin(&p)?;
    let db = app.db.lock().await;
    Ok(Json(json!(db
        .api_keys
        .iter()
        .map(|k| json!({"id":k.id,"name":k.name,"username":k.username}))
        .collect::<Vec<_>>())))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct KeyInput {
    username: String,
    name: String,
}
async fn add_key(
    State(app): State<Arc<App>>,
    Extension(p): Extension<Principal>,
    Json(input): Json<KeyInput>,
) -> Result<Json<Value>> {
    admin(&p)?;
    let mut db = app.db.lock().await;
    let key = auth::issue_key(&mut db, &input.username, &input.name)?;
    db.save(&app.cfg.data_dir)?;
    Ok(Json(json!({"key":key})))
}
async fn delete_key(
    State(app): State<Arc<App>>,
    Extension(p): Extension<Principal>,
    Path(id): Path<String>,
) -> Result<Json<Value>> {
    admin(&p)?;
    let mut db = app.db.lock().await;
    db.api_keys.retain(|k| k.id != id);
    db.save(&app.cfg.data_dir)?;
    Ok(Json(json!({"ok":true})))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct TimeInput {
    unix_seconds: i64,
}
async fn set_time(
    State(app): State<Arc<App>>,
    Extension(p): Extension<Principal>,
    Json(input): Json<TimeInput>,
) -> Result<Json<Value>> {
    admin(&p)?;
    let _permit = app.commands.try_acquire().context("device busy")?;
    app.hw(
        Hardware::SetTime {
            unix_seconds: input.unix_seconds,
        },
        &[],
    )
    .await?;
    Ok(Json(json!({"ok":true})))
}
async fn network_status(
    State(app): State<Arc<App>>,
    Extension(p): Extension<Principal>,
) -> Result<Json<Value>> {
    admin(&p)?;
    Ok(Json(app.hw(Hardware::WifiStatus, &[]).await?.value))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WifiInput {
    ssid: String,
    passphrase: String,
}
async fn network_configure(
    State(app): State<Arc<App>>,
    Extension(p): Extension<Principal>,
    Json(input): Json<WifiInput>,
) -> Result<Json<Value>> {
    admin(&p)?;
    let result = app
        .hw(
            Hardware::WifiConfigure {
                ssid: input.ssid,
                passphrase: input.passphrase,
            },
            &[],
        )
        .await?;
    Ok(Json(result.value))
}
async fn network_recover(
    State(app): State<Arc<App>>,
    Extension(p): Extension<Principal>,
) -> Result<Json<Value>> {
    admin(&p)?;
    app.hw(Hardware::WifiRecover, &[]).await?;
    Ok(Json(json!({"ok":true})))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WifiConfirmInput {
    transaction_id: String,
}
async fn network_confirm(
    State(app): State<Arc<App>>,
    Extension(p): Extension<Principal>,
    Json(input): Json<WifiConfirmInput>,
) -> Result<Json<Value>> {
    admin(&p)?;
    Ok(Json(
        app.hw(
            Hardware::WifiConfirm {
                transaction_id: input.transaction_id,
            },
            &[],
        )
        .await?
        .value,
    ))
}
async fn network_rollback(
    State(app): State<Arc<App>>,
    Extension(p): Extension<Principal>,
) -> Result<Json<Value>> {
    admin(&p)?;
    Ok(Json(app.hw(Hardware::WifiRollback, &[]).await?.value))
}

async fn stats(State(app): State<Arc<App>>) -> Json<Value> {
    Json(crate::stats::collect(&app.cfg))
}

async fn setup_status(State(app): State<Arc<App>>) -> Result<Json<Value>> {
    Ok(Json(
        json!({"required":app.setup_required().await?,"ssid":"furbo2","network_only":!app.cfg.simulate}),
    ))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SetupPassword {
    password: String,
    unix_seconds: Option<i64>,
}
async fn setup_password(
    State(app): State<Arc<App>>,
    ConnectInfo(peer): ConnectInfo<std::net::SocketAddr>,
    Json(input): Json<SetupPassword>,
) -> Result<Json<Value>> {
    if !app.setup_required().await? {
        return Err(Error(
            StatusCode::CONFLICT,
            "password setup is already complete".into(),
        ));
    }
    let auth_state = app.hw(Hardware::AuthState, &[]).await?.value;
    let on_ap = match peer.ip() {
        std::net::IpAddr::V4(ip) => ip.octets()[..3] == [192, 168, 42],
        _ => false,
    };
    ensure!(
        app.cfg.simulate && peer.ip().is_loopback()
            || on_ap && auth_state["recovery_active"] == true,
        "password setup is only available on the recovery Wi-Fi"
    );
    app::rate_limit(&app.login_rate, 6).await?;
    let _hash = app.passwords.try_acquire().context("password setup busy")?;
    let _commands = app.commands.try_acquire().context("device busy")?;
    let user =
        tokio::task::spawn_blocking(move || auth::user(&input.password, Role::Admin)).await??;
    if let Some(unix_seconds) = input.unix_seconds {
        app.hw(Hardware::SetTime { unix_seconds }, &[]).await?;
    }
    let secret = auth::random(48).into_bytes();
    {
        let mut db = app.db.lock().await;
        db.users.clear();
        db.api_keys.clear();
        db.users.insert("admin".into(), user);
        db.save(&app.cfg.data_dir)?;
        crate::store::atomic_write(&app.cfg.data_dir.join("jwt-secret"), &secret)?;
        *app.secret.lock().await = secret;
    }
    app.setup_tokens.lock().await.clear();
    app.hw(
        Hardware::PasswordSetupComplete {
            generation: auth_state["generation"].as_str().map(str::to_owned),
        },
        &[],
    )
    .await?;
    Ok(Json(json!({"ok":true,"username":"admin"})))
}
async fn upgrade_session(
    State(app): State<Arc<App>>,
    Extension(p): Extension<Principal>,
) -> Result<Json<Value>> {
    let db = app.db.lock().await;
    let user = db.users.get(&p.username).context("account missing")?;
    Ok(Json(
        json!({"token":auth::issue_jwt(&p.username,user,&app.secret.lock().await,&app.cfg.device_id)?,"expires_in":auth::SESSION_SECONDS}),
    ))
}

#[cfg(test)]
mod integration_tests {
    use super::*;
    use axum::{body::to_bytes, http::Request as HttpRequest};
    use std::{
        net::{IpAddr, Ipv4Addr, SocketAddr},
        path::PathBuf,
    };
    use tower::ServiceExt;
    fn app() -> (Arc<App>, PathBuf) {
        let dir = std::env::temp_dir().join(format!("furbo-tests-{}", auth::random(9)));
        let mut cfg: crate::config::Config =
            serde_json::from_str(include_str!("../config.example.json")).unwrap();
        cfg.data_dir = dir.clone();
        cfg.http.https_listen = None;
        cfg.simulate = true;
        crate::store::private_dir(&dir).unwrap();
        crate::store::private_dir(&dir.join("media")).unwrap();
        crate::store::Database::default().save(&dir).unwrap();
        crate::store::atomic_write(&dir.join("jwt-secret"), auth::random(48).as_bytes()).unwrap();
        (App::open(cfg).unwrap(), dir)
    }
    async fn call(
        app: &Arc<App>,
        method: &str,
        path: &str,
        token: Option<&str>,
        body: Option<Value>,
        peer: Ipv4Addr,
    ) -> (StatusCode, Value) {
        let mut b = HttpRequest::builder().method(method).uri(path);
        if let Some(token) = token {
            b = b.header(header::AUTHORIZATION, format!("Bearer {token}"));
        }
        if body.is_some() {
            b = b.header(header::CONTENT_TYPE, "application/json");
        }
        let mut req = b
            .body(Body::from(
                body.map(|v| serde_json::to_vec(&v).unwrap())
                    .unwrap_or_default(),
            ))
            .unwrap();
        req.extensions_mut()
            .insert(ConnectInfo(SocketAddr::new(IpAddr::V4(peer), 12345)));
        let response = router(app.clone()).oneshot(req).await.unwrap();
        let code = response.status();
        let bytes = to_bytes(response.into_body(), 2_097_152).await.unwrap();
        let value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
        (code, value)
    }
    #[tokio::test]
    async fn setup_auth_roles_and_persistent_replay() {
        let (app, dir) = app();
        let local = Ipv4Addr::LOCALHOST;
        assert_eq!(
            call(&app, "GET", "/api/v1/setup/status", None, None, local)
                .await
                .1["required"],
            true
        );
        let setup = json!({"password":"test-owner-password"});
        assert_eq!(
            call(
                &app,
                "POST",
                "/api/v1/setup/password",
                None,
                Some(setup.clone()),
                Ipv4Addr::new(10, 0, 0, 2)
            )
            .await
            .0,
            StatusCode::BAD_REQUEST
        );
        assert_eq!(
            call(
                &app,
                "POST",
                "/api/v1/setup/password",
                None,
                Some(setup.clone()),
                local
            )
            .await
            .0,
            StatusCode::OK
        );
        assert_eq!(
            call(
                &app,
                "POST",
                "/api/v1/setup/password",
                None,
                Some(setup),
                local
            )
            .await
            .0,
            StatusCode::CONFLICT
        );
        assert_eq!(
            call(&app, "GET", "/api/v1/stats", None, None, local)
                .await
                .0,
            StatusCode::UNAUTHORIZED
        );
        let (_, login) = call(
            &app,
            "POST",
            "/api/v1/login",
            None,
            Some(json!({"username":"admin","password":"test-owner-password"})),
            local,
        )
        .await;
        let token = login["token"].as_str().unwrap().to_owned();
        assert_eq!(
            call(&app, "GET", "/api/v1/stats", Some(&token), None, local)
                .await
                .0,
            StatusCode::OK
        );
        assert_eq!(
            call(
                &app,
                "POST",
                "/api/v1/users",
                Some(&token),
                Some(
                    json!({"username":"reader","password":"test-reader-password","role":"viewer"})
                ),
                local
            )
            .await
            .0,
            StatusCode::OK
        );
        let (_, key) = call(
            &app,
            "POST",
            "/api/v1/keys",
            Some(&token),
            Some(json!({"username":"reader","name":"read-only-test"})),
            local,
        )
        .await;
        let key = key["key"].as_str().unwrap().to_owned();
        assert_eq!(
            call(&app, "GET", "/api/v1/status", Some(&key), None, local)
                .await
                .0,
            StatusCode::OK
        );
        let cmd =
            json!({"id":"same-toss","issued_at":crate::model::now(),"action":{"type":"toss"}});
        assert_eq!(
            call(
                &app,
                "POST",
                "/api/v1/commands",
                Some(&key),
                Some(cmd.clone()),
                local
            )
            .await
            .0,
            StatusCode::FORBIDDEN
        );
        assert_eq!(app.db.lock().await.last_feed, 0);
        let first = call(
            &app,
            "POST",
            "/api/v1/commands",
            Some(&token),
            Some(cmd.clone()),
            local,
        )
        .await
        .1;
        assert_eq!(first["ok"], true);
        let last_feed = app.db.lock().await.last_feed;
        let restarted = App::open(app.cfg.clone()).unwrap();
        let second = call(
            &restarted,
            "POST",
            "/api/v1/commands",
            Some(&token),
            Some(cmd),
            local,
        )
        .await
        .1;
        assert_eq!(first, second);
        assert_eq!(restarted.db.lock().await.last_feed, last_feed);
        assert_eq!(restarted.db.lock().await.seen.len(), 1);
        let stale = json!({"id":"bad-time","issued_at":i64::MIN,"action":{"type":"toss"}});
        assert_eq!(
            call(
                &app,
                "POST",
                "/api/v1/commands",
                Some(&token),
                Some(stale),
                local
            )
            .await
            .0,
            StatusCode::BAD_REQUEST
        );
        assert_eq!(
            call(
                &app,
                "PUT",
                "/api/v1/users/reader",
                Some(&token),
                Some(json!({"disabled":true})),
                local
            )
            .await
            .0,
            StatusCode::OK
        );
        assert_eq!(
            call(&app, "GET", "/api/v1/status", Some(&key), None, local)
                .await
                .0,
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            call(
                &app,
                "PUT",
                "/api/v1/users/admin",
                Some(&token),
                Some(json!({"disabled":true})),
                local
            )
            .await
            .0,
            StatusCode::BAD_REQUEST
        );
        std::fs::remove_dir_all(dir).unwrap();
    }
    #[tokio::test]
    async fn settings_redact_secrets_validate_and_persist_json() {
        let (original, dir) = app();
        let local = Ipv4Addr::LOCALHOST;
        let (code, _) = call(
            &original,
            "POST",
            "/api/v1/setup/password",
            None,
            Some(json!({"password":"test-settings-password"})),
            local,
        )
        .await;
        assert_eq!(code, StatusCode::OK);
        let mut cfg = original.cfg.clone();
        cfg.mqtt.password = Some("synthetic-broker-secret-47c2".into());
        let app = App::open(cfg.clone()).unwrap();
        let path = dir.join("config.json");
        crate::store::atomic_write(&path, &serde_json::to_vec(&cfg).unwrap()).unwrap();
        *app.config_path.lock().await = Some(path.clone());
        let (_, login) = call(
            &app,
            "POST",
            "/api/v1/login",
            None,
            Some(json!({"username":"admin","password":"test-settings-password"})),
            local,
        )
        .await;
        let token = login["token"].as_str().unwrap();
        assert_eq!(call(&app,"POST","/api/v1/users",Some(token),
            Some(json!({"username":"second-admin","password":"test-second-admin-password","role":"admin"})),local).await.0,StatusCode::OK);
        assert_eq!(
            call(
                &app,
                "PUT",
                "/api/v1/users/admin",
                Some(token),
                Some(json!({"disabled":true})),
                local
            )
            .await
            .0,
            StatusCode::BAD_REQUEST
        );
        assert_eq!(
            call(
                &app,
                "PUT",
                "/api/v1/users/admin",
                Some(token),
                Some(json!({"role":"viewer"})),
                local
            )
            .await
            .0,
            StatusCode::BAD_REQUEST
        );
        assert_eq!(
            call(&app, "GET", "/api/v1/settings", None, None, local)
                .await
                .0,
            StatusCode::UNAUTHORIZED
        );
        let (_, view) = call(&app, "GET", "/api/v1/settings", Some(token), None, local).await;
        assert!(view["config"]["mqtt"]["password"].is_null());
        assert_eq!(view["secrets"]["mqtt_password"], true);
        assert!(!view.to_string().contains("synthetic-broker-secret-47c2"));
        assert_eq!(
            call(
                &app,
                "PUT",
                "/api/v1/settings",
                Some(token),
                Some(json!({"timezone":"Not/A/Timezone"})),
                local
            )
            .await
            .0,
            StatusCode::BAD_REQUEST
        );
        assert_eq!(
            call(
                &app,
                "PUT",
                "/api/v1/settings",
                Some(token),
                Some(json!({"server_certificate_pem":"invalid"})),
                local
            )
            .await
            .0,
            StatusCode::BAD_REQUEST
        );
        let mut media = view["config"]["media"].clone();
        media["retention_days"] = json!(7);
        let (code, saved) = call(
            &app,
            "PUT",
            "/api/v1/settings",
            Some(token),
            Some(json!({"media":media})),
            local,
        )
        .await;
        assert_eq!(code, StatusCode::OK);
        assert_eq!(saved["restart_required"], true);
        let persisted = crate::config::Config::load(&path).unwrap();
        assert_eq!(persisted.media.retention_days, 7);
        assert_eq!(persisted.mqtt.password, cfg.mqtt.password);
        assert_eq!(
            call(&app, "GET", "/api/v1/logs.ndjson", Some(token), None, local)
                .await
                .0,
            StatusCode::OK
        );
        assert!(!crate::logging::download().contains("synthetic-broker-secret-47c2"));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test]
    async fn retention_expires_old_files_and_preserves_scheduled_voice() {
        let (original, dir) = app();
        let mut cfg = original.cfg.clone();
        cfg.media.retention_days = 1;
        let app = App::open(cfg).unwrap();
        for name in ["old.jpg", "voice.wav", "recent.jpg"] {
            crate::store::atomic_write(&dir.join("media").join(name), b"synthetic").unwrap();
        }
        let old =
            std::time::UNIX_EPOCH + Duration::from_secs((crate::model::now() - 172800) as u64);
        for name in ["old.jpg", "voice.wav"] {
            std::fs::File::open(dir.join("media").join(name))
                .unwrap()
                .set_times(std::fs::FileTimes::new().set_modified(old))
                .unwrap();
        }
        app.db.lock().await.schedules.push(Schedule {
            id: "breakfast".into(),
            time: "08:00".into(),
            days: vec![0],
            enabled: true,
            voice_id: Some("voice".into()),
        });
        assert_eq!(app.prune_media(0).await.unwrap(), 1);
        assert!(!dir.join("media/old.jpg").exists());
        assert!(dir.join("media/voice.wav").exists());
        assert!(dir.join("media/recent.jpg").exists());
        std::fs::remove_dir_all(dir).unwrap();
    }
}
