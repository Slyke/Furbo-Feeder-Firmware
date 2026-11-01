mod api;
mod app;
mod auth;
mod background;
mod config;
mod hardware;
mod logging;
mod model;
mod mp4;
mod mqtt;
mod network;
mod portal;
mod recovery;
mod rtsp;
mod settings;
mod stats;
mod store;
mod timezone;
mod tls;
mod watchdog;
use anyhow::{ensure, Context, Result};
use config::Config;
use std::{
    path::{Path, PathBuf},
    time::Duration,
};
fn main() -> Result<()> {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .max_blocking_threads(2)
        .build()?;
    runtime.block_on(run())
}
async fn run() -> Result<()> {
    let args: Vec<_> = std::env::args().collect();
    match args.get(1).map(String::as_str) {
        Some("--version")=>println!("furbo-local {}",env!("CARGO_PKG_VERSION")),
        Some("init")=>{
            let path=Path::new(args.get(2).context("usage: furbo-local init config.json")?);let cfg=Config::load(path)?;
            ensure!(!cfg.data_dir.join("state.json").exists(),"already initialized; refusing to overwrite credentials");
            store::private_dir(&cfg.data_dir)?;store::private_dir(&media_dir(&cfg))?;
            let db=store::Database::default();db.save(&cfg.data_dir)?;
            store::atomic_write(&cfg.data_dir.join("jwt-secret"),auth::random(48).as_bytes())?;
            println!("Initialized with no password. Open the setup page to create an administrator password.");
        },
        Some("check-config")=>{Config::load(Path::new(args.get(2).context("config path required")?))?;println!("configuration valid");},
        Some("factory-reset")=>network::factory_reset()?,
        Some("network-init")=>network::init()?,
        Some("network")=>network::serve().await?,
        Some("hardware")=>{
            let socket=PathBuf::from(args.get(2).context("usage: furbo-local hardware SOCKET [UID] [GID]")?);
            let uid=args.get(3).map(|v|v.parse()).transpose()?.unwrap_or(33);
            let gid=args.get(4).map(|v|v.parse()).transpose()?.unwrap_or(33);
            hardware::serve(socket,uid,gid).await?;
        },
        Some("serve")=>{
            let config_path=std::fs::canonicalize(args.get(2).context("usage: furbo-local serve config.json")?)?;
            let cfg=Config::load(&config_path)?;
            let app=app::App::open(cfg)?;*app.config_path.lock().await=Some(config_path);
            crate::logging::record("info","started",serde_json::json!({"version":env!("CARGO_PKG_VERSION")}));
            let router=api::router(app.clone());
            background::start(app.clone());
            if app.cfg.mqtt.enabled {
                let a=app.clone();tokio::spawn(async move {if mqtt::run(a).await.is_err(){crate::log_error!("MQTT configuration failed; check broker and certificates");}});
            }
            let a=app.clone();tokio::spawn(async move {
                for _ in 0..36 {
                    let (volume,muted,night)={let db=a.db.lock().await;(db.volume,db.muted,db.night.clone())};
                    if a.hw(hardware::Request::Volume{percent:volume},&[]).await.is_ok()
                        && a.hw(hardware::Request::Mute{muted},&[]).await.is_ok()
                        && a.hw(hardware::Request::Night{mode:night},&[]).await.is_ok()
                        && a.hw(hardware::Request::Overlay{label:a.cfg.camera.overlay_text.clone(),show_timestamp:a.cfg.camera.show_timestamp},&[]).await.is_ok(){break;}
                    tokio::time::sleep(Duration::from_secs(5)).await;
                }
            });
            let handle=axum_server::Handle::new();let stop=handle.clone();
            let (shutdown_tx,mut shutdown_rx)=tokio::sync::watch::channel(false);
            tokio::spawn(async move {
                let mut term=tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()).expect("SIGTERM");
                tokio::select!{_=tokio::signal::ctrl_c()=>{},_=term.recv()=>{}}
                stop.graceful_shutdown(Some(Duration::from_secs(8)));
                let _=shutdown_tx.send(true);
            });
            let http_enabled=app.cfg.http.http_enabled;
            let https_address=app.cfg.http.https_listen.filter(|_|app.cfg.http.https_enabled);
            if !http_enabled && https_address.is_none() {
                // MQTT-only mode still runs background jobs and handles shutdown.
                shutdown_rx.changed().await?;
            } else {
                let http_router=router.clone();let http_handle=handle.clone();
                let http=async {
                    if http_enabled {
                        axum_server::bind(app.cfg.http.listen).handle(http_handle).serve(http_router.into_make_service_with_connect_info::<std::net::SocketAddr>()).await
                    } else {std::future::pending::<std::io::Result<()>>().await}
                };
                let https=async {
                    if let Some(address)=https_address {
                        let tls=axum_server::tls_rustls::RustlsConfig::from_pem_file(app.cfg.http.certificate.as_ref().unwrap(),app.cfg.http.private_key.as_ref().unwrap()).await?;
                        axum_server::bind_rustls(address,tls).handle(handle).serve(router.into_make_service_with_connect_info::<std::net::SocketAddr>()).await
                    } else {std::future::pending::<std::io::Result<()>>().await}
                };
                tokio::select!{r=http=>r?,r=https=>r?}
            }
        },
        _=>anyhow::bail!("usage: furbo-local init|serve|check-config CONFIG; furbo-local hardware SOCKET [UID] [GID]; furbo-local --version"),
    }
    Ok(())
}
pub fn media_dir(cfg: &Config) -> PathBuf {
    cfg.media
        .directory
        .clone()
        .unwrap_or_else(|| cfg.data_dir.join("media"))
}
