//! Web recovery defaults used by the physical reset path.
use anyhow::Result;
use std::path::Path;
pub fn write_owned(path: &Path, bytes: &[u8]) -> Result<()> {
    crate::store::atomic_write(path, bytes)?;
    if unsafe { libc::geteuid() } == 0 {
        use std::os::unix::ffi::OsStrExt;
        let name = std::ffi::CString::new(path.as_os_str().as_bytes())?;
        anyhow::ensure!(
            unsafe { libc::chown(name.as_ptr(), 33, 33) } == 0,
            "cannot set device file ownership"
        );
    }
    Ok(())
}
pub fn web_defaults(cfg: &mut crate::config::Config) -> Result<()> {
    let cert = cfg.data_dir.join("recovery-server.crt");
    let key = cfg.data_dir.join("recovery-server.key");
    if !cert.exists() || !key.exists() {
        let generated = rcgen::generate_simple_self_signed(vec![
            "furbo.local".into(),
            "furbo2.local".into(),
            "localhost".into(),
            "127.0.0.1".into(),
            "192.168.42.1".into(),
        ])?;
        write_owned(&cert, generated.cert.pem().as_bytes())?;
        write_owned(&key, generated.signing_key.serialize_pem().as_bytes())?;
    }
    cfg.http.max_connections = 4;
    cfg.http.http_enabled = true;
    cfg.http.https_enabled = true;
    cfg.http.listen = "0.0.0.0:8080".parse()?;
    cfg.http.allow_insecure_lan = true;
    cfg.http.https_listen = Some("0.0.0.0:8443".parse()?);
    cfg.http.certificate = Some(cert);
    cfg.http.private_key = Some(key);
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn reset_restores_both_listeners_without_changing_integrations() {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let dir = std::env::temp_dir().join(format!("furbo-recovery-{}", crate::auth::random(9)));
        crate::store::private_dir(&dir).unwrap();
        let mut cfg: crate::config::Config =
            serde_json::from_str(include_str!("../config.example.json")).unwrap();
        cfg.data_dir = dir.clone();
        cfg.http.http_enabled = false;
        cfg.http.https_enabled = false;
        cfg.http.listen = "127.0.0.1:12345".parse().unwrap();
        cfg.http.https_listen = Some("127.0.0.1:23456".parse().unwrap());
        cfg.mqtt.password = Some("synthetic-preserved-password".into());
        web_defaults(&mut cfg).unwrap();
        assert!(cfg.http.http_enabled && cfg.http.https_enabled);
        assert_eq!(cfg.http.listen.to_string(), "0.0.0.0:8080");
        assert_eq!(cfg.http.https_listen.unwrap().to_string(), "0.0.0.0:8443");
        assert_eq!(
            cfg.mqtt.password.as_deref(),
            Some("synthetic-preserved-password")
        );
        axum_server::tls_rustls::RustlsConfig::from_pem_file(
            cfg.http.certificate.unwrap(),
            cfg.http.private_key.unwrap(),
        )
        .await
        .unwrap();
        std::fs::remove_dir_all(dir).unwrap();
    }
}
