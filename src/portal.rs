//! Bounded recovery-only DNS and HTTP redirect. Credentials go to HTTPS.
use anyhow::Result;
use std::{sync::Arc, time::Duration};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, UdpSocket},
    sync::Semaphore,
    task::JoinHandle,
};
pub struct Portal(Vec<JoinHandle<()>>);
impl Drop for Portal {
    fn drop(&mut self) {
        for task in &self.0 {
            task.abort();
        }
    }
}
fn dns_response(query: &[u8]) -> Option<Vec<u8>> {
    if query.len() < 12 || query.len() > 512 || query[2] & 0x80 != 0 || query[4..6] != [0, 1] {
        return None;
    }
    let mut at = 12;
    loop {
        let n = *query.get(at)? as usize;
        at += 1;
        if n == 0 {
            break;
        }
        if n > 63 || at + n > query.len() {
            return None;
        }
        at += n;
    }
    if at + 4 > query.len() {
        return None;
    }
    let kind = u16::from_be_bytes([query[at], query[at + 1]]);
    let class = u16::from_be_bytes([query[at + 2], query[at + 3]]);
    at += 4;
    if class != 1 {
        return None;
    }
    let answer = kind == 1;
    let mut out = query[..at].to_vec();
    out[2] = 0x81;
    out[3] = 0x80;
    out[6] = 0;
    out[7] = u8::from(answer);
    out[8..12].fill(0);
    if answer {
        out.extend([0xc0, 0x0c, 0, 1, 0, 1, 0, 0, 0, 30, 0, 4, 192, 168, 42, 1]);
    }
    Some(out)
}
fn recovery_url(http: &crate::config::Http) -> Result<String> {
    if let Some(address) = http
        .https_listen
        .filter(|address| address.ip().is_unspecified())
    {
        Ok(format!("https://192.168.42.1:{}/", address.port()))
    } else if http.http_enabled && http.allow_insecure_lan && http.listen.ip().is_unspecified() {
        Ok(format!("http://192.168.42.1:{}/", http.listen.port()))
    } else {
        anyhow::bail!("no recovery-accessible listener")
    }
}
pub async fn start() -> Result<Portal> {
    let cfg = crate::config::Config::load(std::path::Path::new(
        "/usr/local/furbo-local/etc/config.json",
    ))?;
    let location = recovery_url(&cfg.http)?;
    let redirect=format!("HTTP/1.1 302 Found\r\nLocation: {location}\r\nCache-Control: no-store\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
    let dns = UdpSocket::bind("192.168.42.1:53").await?;
    let http = TcpListener::bind("192.168.42.1:80").await?;
    let dns_task = tokio::spawn(async move {
        let mut buffer = [0; 512];
        while let Ok((n, peer)) = dns.recv_from(&mut buffer).await {
            if let Some(out) = dns_response(&buffer[..n]) {
                let _ = dns.send_to(&out, peer).await;
            }
        }
    });
    let http_task = tokio::spawn(async move {
        let gate = Arc::new(Semaphore::new(4));
        while let Ok((mut stream, _)) = http.accept().await {
            let Ok(permit) = gate.clone().try_acquire_owned() else {
                continue;
            };
            let redirect = redirect.clone();
            tokio::spawn(async move {
                let _permit = permit;
                let mut request = [0; 1024];
                let _ = tokio::time::timeout(Duration::from_secs(2), async {
                    if stream.read(&mut request).await? == 0 {
                        return std::io::Result::Ok(());
                    }
                    stream.write_all(redirect.as_bytes()).await?;
                    std::io::Result::Ok(())
                })
                .await;
            });
        }
    });
    Ok(Portal(vec![dns_task, http_task]))
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn recovery_follows_configured_listener() {
        let mut cfg: crate::config::Config =
            serde_json::from_str(include_str!("../config.example.json")).unwrap();
        cfg.http.https_listen = Some("0.0.0.0:9443".parse().unwrap());
        assert_eq!(
            recovery_url(&cfg.http).unwrap(),
            "https://192.168.42.1:9443/"
        );
        cfg.http.https_listen = None;
        cfg.http.listen = "0.0.0.0:8080".parse().unwrap();
        cfg.http.allow_insecure_lan = true;
        assert_eq!(
            recovery_url(&cfg.http).unwrap(),
            "http://192.168.42.1:8080/"
        );
    }
    #[test]
    fn bounded_dns() {
        let q = [
            0x12, 0x34, 1, 0, 0, 1, 0, 0, 0, 0, 0, 0, 1, b'a', 0, 0, 1, 0, 1,
        ];
        let r = dns_response(&q).unwrap();
        assert_eq!(&r[r.len() - 4..], &[192, 168, 42, 1]);
        assert_eq!(r[7], 1);
        assert!(dns_response(&q[..14]).is_none());
        let mut bad = q;
        bad[12] = 0xc0;
        assert!(dns_response(&bad).is_none());
    }
}
