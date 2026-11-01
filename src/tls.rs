//! Normal PKI verification, with optional remembered trust for private/self-signed peers.
use anyhow::{ensure, Result};
use rustls::{
    client::{
        danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier},
        WebPkiServerVerifier,
    },
    pki_types::{CertificateDer, ServerName, UnixTime},
    RootCertStore,
};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    io::BufReader,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::Duration,
};
static PIN_LOCK: Mutex<()> = Mutex::new(());
pub fn roots(path: &Option<PathBuf>) -> Result<Arc<RootCertStore>> {
    let mut roots = RootCertStore::empty();
    roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    if let Some(path) = path {
        let pem = std::fs::read(path)?;
        ensure!(pem.len() <= 1_048_576, "CA file too large");
        let certs = rustls_pemfile::certs(&mut BufReader::new(pem.as_slice()))
            .collect::<std::io::Result<Vec<_>>>()?;
        ensure!(!certs.is_empty(), "empty CA file");
        for cert in certs {
            roots.add(cert)?;
        }
    }
    Ok(Arc::new(roots))
}
#[derive(Debug)]
struct RememberedVerifier {
    inner: Arc<WebPkiServerVerifier>,
    roots: Arc<RootCertStore>,
    name: Option<ServerName<'static>>,
    allow: bool,
    key: String,
    path: PathBuf,
}
fn tls_error(message: &str) -> rustls::Error {
    rustls::Error::General(message.into())
}
fn pins(path: &Path) -> Result<BTreeMap<String, String>> {
    match std::fs::metadata(path) {
        Ok(meta) => {
            ensure!(meta.len() <= 16384, "invalid certificate memory file");
            Ok(serde_json::from_slice(&std::fs::read(path)?)?)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(BTreeMap::new()),
        Err(error) => Err(error.into()),
    }
}
impl ServerCertVerifier for RememberedVerifier {
    fn verify_server_cert(
        &self,
        end: &CertificateDer<'_>,
        chain: &[CertificateDer<'_>],
        name: &ServerName<'_>,
        ocsp: &[u8],
        now: UnixTime,
    ) -> std::result::Result<ServerCertVerified, rustls::Error> {
        let name = match &self.name {
            Some(override_name) => override_name,
            None => name,
        };
        match self.inner.verify_server_cert(end, chain, name, ocsp, now) {
            Ok(verified) => return Ok(verified),
            Err(rustls::Error::InvalidCertificate(rustls::CertificateError::UnknownIssuer))
                if self.allow => {}
            Err(error) => return Err(error),
        }
        if chain.len() > 8
            || end.len() + chain.iter().map(|cert| cert.len()).sum::<usize>() > 131072
        {
            return Err(tls_error("certificate chain exceeds private-trust limit"));
        }
        // Trust only anchors supplied by this peer, then let WebPKI check the
        // complete chain, expiry, DNS/IP name and certificate signatures.
        let mut candidates = (*self.roots).clone();
        candidates.add(end.clone().into_owned())?;
        for cert in chain {
            candidates.add(cert.clone().into_owned())?;
        }
        let verifier = WebPkiServerVerifier::builder(Arc::new(candidates))
            .build()
            .map_err(|_| tls_error("cannot build private-certificate verifier"))?;
        let verified = verifier.verify_server_cert(end, chain, name, ocsp, now)?;
        let fingerprint = Sha256::digest(end.as_ref())
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>();
        let _guard = PIN_LOCK
            .lock()
            .map_err(|_| tls_error("certificate memory unavailable"))?;
        let mut known =
            pins(&self.path).map_err(|_| tls_error("cannot read remembered certificates"))?;
        if let Some(previous) = known.get(&self.key) {
            if previous != &fingerprint {
                return Err(tls_error(
                    "self-signed certificate changed; clear remembered trust to reconnect",
                ));
            }
        } else {
            if known.len() >= 32 {
                return Err(tls_error("certificate memory full; clear unused trust"));
            }
            known.insert(self.key.clone(), fingerprint);
            crate::store::atomic_write(
                &self.path,
                &serde_json::to_vec(&known)
                    .map_err(|_| tls_error("cannot encode certificate memory"))?,
            )
            .map_err(|_| tls_error("cannot remember self-signed certificate"))?;
        }
        Ok(verified)
    }
    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        signature: &rustls::DigitallySignedStruct,
    ) -> std::result::Result<HandshakeSignatureValid, rustls::Error> {
        self.inner.verify_tls12_signature(message, cert, signature)
    }
    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        signature: &rustls::DigitallySignedStruct,
    ) -> std::result::Result<HandshakeSignatureValid, rustls::Error> {
        self.inner.verify_tls13_signature(message, cert, signature)
    }
    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        self.inner.supported_verify_schemes()
    }
}
pub fn verifier(
    roots: Arc<RootCertStore>,
    name: Option<String>,
    allow: bool,
    key: String,
    data: &Path,
) -> Result<Arc<dyn ServerCertVerifier>> {
    Ok(Arc::new(RememberedVerifier {
        inner: WebPkiServerVerifier::builder(roots.clone()).build()?,
        roots,
        name: name.map(ServerName::try_from).transpose()?,
        allow,
        key,
        path: data.join("tls-pins.json"),
    }))
}
pub fn http_client(
    cfg: &crate::config::Config,
    url: Option<&str>,
    allow_self_signed: bool,
) -> Result<reqwest::Client> {
    let roots = roots(&cfg.media.ca_file)?;
    let mut tls = rustls::ClientConfig::builder()
        .with_root_certificates(roots.clone())
        .with_no_client_auth();
    let key = url
        .and_then(|value| reqwest::Url::parse(value).ok())
        .map(|url| {
            format!(
                "https://{}:{}",
                url.host_str().unwrap_or(""),
                url.port_or_known_default().unwrap_or(443)
            )
        })
        .unwrap_or_else(|| "unused".into());
    tls.dangerous().set_certificate_verifier(verifier(
        roots,
        None,
        allow_self_signed,
        key,
        &cfg.data_dir,
    )?);
    Ok(reqwest::Client::builder()
        .use_preconfigured_tls(tls)
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(30))
        .connect_timeout(Duration::from_secs(5))
        .pool_max_idle_per_host(1)
        .build()?)
}
pub fn forget(data: &Path) -> Result<()> {
    let _guard = PIN_LOCK
        .lock()
        .map_err(|_| anyhow::anyhow!("certificate memory unavailable"))?;
    match std::fs::remove_file(data.join("tls-pins.json")) {
        Ok(()) => {
            std::fs::File::open(data)?.sync_all()?;
            Ok(())
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn optional_private_trust_checks_names_time_and_certificate_changes() {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let dir = std::env::temp_dir().join(format!("furbo-tls-{}", crate::auth::random(9)));
        crate::store::private_dir(&dir).unwrap();
        let first = rcgen::generate_simple_self_signed(vec!["localhost".into()]).unwrap();
        let second = rcgen::generate_simple_self_signed(vec!["localhost".into()]).unwrap();
        let name = ServerName::try_from("localhost").unwrap();
        let now = UnixTime::now();
        let roots = roots(&None).unwrap();
        let strict = verifier(roots.clone(), None, false, "test".into(), &dir).unwrap();
        assert!(strict
            .verify_server_cert(first.cert.der(), &[], &name, &[], now)
            .is_err());
        let allowed = verifier(roots, None, true, "test".into(), &dir).unwrap();
        assert!(allowed
            .verify_server_cert(first.cert.der(), &[], &name, &[], now)
            .is_ok());
        assert!(allowed
            .verify_server_cert(first.cert.der(), &[], &name, &[], now)
            .is_ok());
        assert!(allowed
            .verify_server_cert(second.cert.der(), &[], &name, &[], now)
            .is_err());
        let wrong = ServerName::try_from("different.invalid").unwrap();
        assert!(allowed
            .verify_server_cert(first.cert.der(), &[], &wrong, &[], now)
            .is_err());
        let early = UnixTime::since_unix_epoch(Duration::ZERO);
        assert!(allowed
            .verify_server_cert(first.cert.der(), &[], &name, &[], early)
            .is_err());
        forget(&dir).unwrap();
        assert!(allowed
            .verify_server_cert(second.cert.der(), &[], &name, &[], now)
            .is_ok());
        std::fs::remove_dir_all(dir).unwrap();
    }
}
