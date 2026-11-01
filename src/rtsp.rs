use crate::mp4;
use anyhow::{bail, ensure, Context, Result};
use base64::{engine::general_purpose::STANDARD, Engine};
use std::{
    collections::BTreeMap,
    time::{Duration, Instant},
};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    net::TcpStream,
};
const URL: &str = "rtsp://127.0.0.1/stream0";
const MAX_AU: usize = 1_048_576;
#[derive(Default)]
pub struct Depacketizer {
    seq: Option<u16>,
    fragment: Vec<u8>,
    frame: Vec<u8>,
    key: bool,
    damaged: bool,
    stamp: Option<u32>,
}
pub struct AccessUnit {
    pub timestamp: u32,
    pub key: bool,
    pub data: Vec<u8>,
}
impl Depacketizer {
    pub fn push(&mut self, p: &[u8]) -> Result<Option<AccessUnit>> {
        ensure!(p.len() >= 12 && p[0] >> 6 == 2, "invalid RTP");
        let sequence = u16::from_be_bytes([p[2], p[3]]);
        let timestamp = u32::from_be_bytes(p[4..8].try_into()?);
        if self.stamp != Some(timestamp) {
            self.frame.clear();
            self.fragment.clear();
            self.key = false;
            self.damaged = false;
            self.stamp = Some(timestamp);
        }
        if self.seq.is_some_and(|s| s.wrapping_add(1) != sequence) {
            self.damaged = true;
            self.fragment.clear();
        }
        self.seq = Some(sequence);
        let mut i = 12 + (p[0] & 15) as usize * 4;
        ensure!(i <= p.len(), "bad RTP CSRC");
        if p[0] & 16 != 0 {
            ensure!(i + 4 <= p.len(), "bad RTP extension");
            let n = u16::from_be_bytes([p[i + 2], p[i + 3]]) as usize * 4;
            i += 4 + n;
        }
        ensure!(i <= p.len(), "invalid RTP extension length");
        let end = if p[0] & 32 != 0 {
            let n = *p.last().unwrap() as usize;
            ensure!(n > 0 && n <= p.len() - i, "bad RTP padding");
            p.len() - n
        } else {
            p.len()
        };
        ensure!(i < end, "empty RTP payload");
        let payload = &p[i..end];
        let kind = payload[0] & 31;
        match kind {
            1..=23 => self.nal(payload)?,
            24 => {
                let mut j = 1;
                while j + 2 <= payload.len() {
                    let n = u16::from_be_bytes([payload[j], payload[j + 1]]) as usize;
                    j += 2;
                    ensure!(n > 0 && j + n <= payload.len(), "bad STAP-A");
                    self.nal(&payload[j..j + n])?;
                    j += n;
                }
                ensure!(j == payload.len(), "truncated STAP-A");
            }
            28 => {
                ensure!(payload.len() >= 3, "bad FU-A");
                if payload[1] & 128 != 0 {
                    self.fragment.clear();
                    self.fragment.push((payload[0] & 0xe0) | (payload[1] & 31));
                } else if self.fragment.is_empty() {
                    self.damaged = true;
                }
                self.fragment.extend_from_slice(&payload[2..]);
                ensure!(self.fragment.len() <= MAX_AU, "NAL too large");
                if payload[1] & 64 != 0 {
                    let nal = std::mem::take(&mut self.fragment);
                    self.nal(&nal)?;
                }
            }
            _ => bail!("unsupported H264 RTP packetization"),
        }
        if p[1] & 128 != 0 {
            let frame = std::mem::take(&mut self.frame);
            if !self.damaged && self.fragment.is_empty() && !frame.is_empty() {
                return Ok(Some(AccessUnit {
                    timestamp,
                    key: self.key,
                    data: frame,
                }));
            }
        }
        Ok(None)
    }
    fn nal(&mut self, n: &[u8]) -> Result<()> {
        ensure!(!n.is_empty(), "empty NAL");
        let kind = n[0] & 31;
        if kind == 7 || kind == 8 {
            return Ok(());
        }
        self.key |= kind == 5;
        ensure!(
            self.frame.len() + n.len() + 4 <= MAX_AU,
            "access unit too large"
        );
        self.frame.extend((n.len() as u32).to_be_bytes());
        self.frame.extend(n);
        Ok(())
    }
}

pub struct Fragment {
    pub bytes: Vec<u8>,
    pub track: u32,
    pub duration: u32,
    pub key: bool,
}

pub struct Session {
    io: BufReader<TcpStream>,
    seq: u32,
    session: String,
    url: String,
    pub init: Vec<u8>,
    pub mime: String,
    dep: Depacketizer,
    video_base: Option<u32>,
    audio_base: Option<u32>,
    fragment_seq: u32,
    pending: Option<AccessUnit>,
    last_keepalive: Instant,
    audio: bool,
}
impl Session {
    pub async fn open(audio: bool) -> Result<Self> {
        let io = BufReader::new(
            tokio::time::timeout(Duration::from_secs(5), TcpStream::connect("127.0.0.1:554"))
                .await??,
        );
        let mut s = Self {
            io,
            seq: 0,
            session: String::new(),
            url: URL.into(),
            init: vec![],
            mime: String::new(),
            dep: Depacketizer::default(),
            video_base: None,
            audio_base: None,
            fragment_seq: 0,
            pending: None,
            last_keepalive: Instant::now(),
            audio,
        };
        let (headers, body) = s
            .request("DESCRIBE", URL, "Accept: application/sdp\r\n")
            .await?;
        let sdp = std::str::from_utf8(&body)?;
        let mut video_control = None;
        let mut audio_control = None;
        let mut is_video = false;
        let mut sps = None;
        let mut pps = None;
        let mut width = 1920;
        let mut height = 1080;
        let mut audio_config = vec![0x14, 0x08];
        for line in sdp.lines() {
            if line.starts_with("m=") {
                is_video = line.starts_with("m=video");
            }
            if let Some(c) = line.strip_prefix("a=control:") {
                if c != "*" {
                    if is_video {
                        video_control = Some(c.to_owned())
                    } else {
                        audio_control = Some(c.to_owned())
                    }
                }
            }
            if let Some((_, sets)) = line.split_once("sprop-parameter-sets=") {
                let value = sets.split(';').next().unwrap_or("");
                if let Some((a, b)) = value.split_once(',') {
                    sps = Some(STANDARD.decode(a)?);
                    pps = Some(STANDARD.decode(b)?);
                }
            }
            if let Some(rect) = line.strip_prefix("a=cliprect:") {
                let p: Vec<_> = rect.split(',').collect();
                if p.len() == 4 {
                    height = p[2].parse()?;
                    width = p[3].parse()?;
                }
            }
            if !is_video {
                if let Some((_, c)) = line.split_once("config=") {
                    let c = c.split(';').next().unwrap_or("").trim();
                    ensure!(c.len() == 4, "only AAC-LC two-byte configuration supported");
                    audio_config = vec![
                        u8::from_str_radix(&c[..2], 16)?,
                        u8::from_str_radix(&c[2..], 16)?,
                    ];
                }
            }
        }
        let sps = sps.context("SDP missing SPS")?;
        let pps = pps.context("SDP missing PPS")?;
        ensure!(
            sps.len() >= 4 && sps.len() <= 1024 && pps.len() <= 1024,
            "bad AVC configuration"
        );
        ensure!(width <= 1920 && height <= 1080, "unsupported resolution");
        s.url = headers.get("content-base").cloned().unwrap_or(URL.into());
        let control = |c: String, base: &str| {
            if c.starts_with("rtsp://") {
                c
            } else {
                format!("{}/{}", base.trim_end_matches('/'), c)
            }
        };
        let v = control(video_control.context("video control")?, &s.url);
        let (h, _) = s
            .request(
                "SETUP",
                &v,
                "Transport: RTP/AVP/TCP;unicast;interleaved=0-1\r\n",
            )
            .await?;
        s.session = h
            .get("session")
            .context("missing RTSP session")?
            .split(';')
            .next()
            .unwrap_or("")
            .to_owned();
        ensure!(
            !s.session.contains(['\r', '\n']) && s.session.len() < 128,
            "invalid RTSP session"
        );
        if audio {
            let a = control(audio_control.context("audio control")?, &s.url);
            s.request(
                "SETUP",
                &a,
                "Transport: RTP/AVP/TCP;unicast;interleaved=2-3\r\n",
            )
            .await?;
        }
        s.init = mp4::init(
            &sps,
            &pps,
            width,
            height,
            if audio {
                Some((&audio_config, 16000, 1))
            } else {
                None
            },
        );
        s.mime = format!(
            "video/mp4; codecs=\"avc1.{:02X}{:02X}{:02X}{}\"",
            sps[1],
            sps[2],
            sps[3],
            if audio { ",mp4a.40.2" } else { "" }
        );
        let url = s.url.clone();
        s.request("PLAY", &url, "Range: npt=0.000-\r\n").await?;
        Ok(s)
    }
    async fn request(
        &mut self,
        method: &str,
        url: &str,
        extra: &str,
    ) -> Result<(BTreeMap<String, String>, Vec<u8>)> {
        ensure!(
            !url.contains(['\r', '\n']) && url.len() < 1024,
            "invalid RTSP control URI"
        );
        self.seq += 1;
        let session = if self.session.is_empty() {
            String::new()
        } else {
            format!("Session: {}\r\n", self.session)
        };
        let msg = format!(
            "{method} {url} RTSP/1.0\r\nCSeq: {}\r\n{session}{extra}\r\n",
            self.seq
        );
        self.io.get_mut().write_all(msg.as_bytes()).await?;
        tokio::time::timeout(Duration::from_secs(5), self.response()).await?
    }
    async fn response(&mut self) -> Result<(BTreeMap<String, String>, Vec<u8>)> {
        let mut head = Vec::new();
        loop {
            let b = self.io.read_u8().await?;
            head.push(b);
            ensure!(head.len() <= 8192, "RTSP headers too large");
            if head.ends_with(b"\r\n\r\n") {
                break;
            }
        }
        let text = std::str::from_utf8(&head)?;
        ensure!(text.starts_with("RTSP/1.0 200"), "RTSP request rejected");
        let mut headers = BTreeMap::new();
        for l in text.lines().skip(1) {
            if let Some((k, v)) = l.split_once(':') {
                headers.insert(k.to_ascii_lowercase(), v.trim().to_owned());
            }
        }
        let n = headers
            .get("content-length")
            .map(|v| v.parse::<usize>())
            .transpose()?
            .unwrap_or(0);
        ensure!(n <= 16384, "SDP too large");
        let mut body = vec![0; n];
        self.io.read_exact(&mut body).await?;
        Ok((headers, body))
    }
    pub async fn next(&mut self) -> Result<Vec<u8>> {
        Ok(self.next_fragment().await?.bytes)
    }
    pub async fn next_fragment(&mut self) -> Result<Fragment> {
        tokio::time::timeout(Duration::from_secs(15), self.next_inner()).await?
    }
    async fn next_inner(&mut self) -> Result<Fragment> {
        loop {
            if self.last_keepalive.elapsed() > Duration::from_secs(20) {
                self.seq += 1;
                let msg = format!(
                    "GET_PARAMETER {} RTSP/1.0\r\nCSeq: {}\r\nSession: {}\r\n\r\n",
                    self.url, self.seq, self.session
                );
                self.io.get_mut().write_all(msg.as_bytes()).await?;
                self.last_keepalive = Instant::now();
            }
            // RTSP responses can be interleaved with '$' RTP records.
            let b = self.io.fill_buf().await?;
            ensure!(!b.is_empty(), "RTSP closed");
            if b[0] != b'$' {
                self.response().await?;
                continue;
            }
            self.io.read_u8().await?;
            let channel = self.io.read_u8().await?;
            let n = self.io.read_u16().await? as usize;
            ensure!(n >= 12, "short interleaved record");
            let mut p = vec![0; n];
            self.io.read_exact(&mut p).await?;
            if channel == 0 {
                if let Some(au) = self.dep.push(&p)? {
                    if self.video_base.is_none() {
                        if !au.key {
                            continue;
                        }
                        self.video_base = Some(au.timestamp);
                    }
                    if let Some(prev) = self.pending.replace(au) {
                        let next = self.pending.as_ref().unwrap().timestamp;
                        let duration = next.wrapping_sub(prev.timestamp);
                        ensure!(
                            duration > 0 && duration < 90000,
                            "non-monotonic video timestamps; B frames unsupported"
                        );
                        self.fragment_seq += 1;
                        return Ok(Fragment {
                            bytes: mp4::fragment(
                                self.fragment_seq,
                                1,
                                prev.timestamp.wrapping_sub(self.video_base.unwrap()) as u64,
                                duration,
                                prev.key,
                                &prev.data,
                            ),
                            track: 1,
                            duration,
                            key: prev.key,
                        });
                    }
                }
            } else if channel == 2 && self.audio && self.video_base.is_some() {
                // RFC 3640 AAC-hbr: one AU, 13-bit size + 3-bit AU index.
                let mut i = 12 + (p[0] & 15) as usize * 4;
                if p[0] & 16 != 0 {
                    ensure!(i + 4 <= p.len(), "bad AAC extension");
                    i += 4 + u16::from_be_bytes([p[i + 2], p[i + 3]]) as usize * 4;
                }
                ensure!(i + 4 <= p.len(), "short AAC packet");
                let bits = u16::from_be_bytes([p[i], p[i + 1]]) as usize;
                ensure!(bits == 16, "only single AAC AU per RTP packet supported");
                let size = (u16::from_be_bytes([p[i + 2], p[i + 3]]) >> 3) as usize;
                i += 4;
                ensure!(size > 0 && i + size <= p.len(), "truncated AAC AU");
                let ts = u32::from_be_bytes(p[4..8].try_into()?);
                let base = *self.audio_base.get_or_insert(ts);
                self.fragment_seq += 1;
                return Ok(Fragment {
                    bytes: mp4::fragment(
                        self.fragment_seq,
                        2,
                        ts.wrapping_sub(base) as u64,
                        1024,
                        true,
                        &p[i..i + size],
                    ),
                    track: 2,
                    duration: 1024,
                    key: true,
                });
            }
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    fn packet(seq: u16, marker: bool, payload: &[u8]) -> Vec<u8> {
        let mut p = vec![0x80, 96 | if marker { 128 } else { 0 }];
        p.extend(seq.to_be_bytes());
        p.extend(90000u32.to_be_bytes());
        p.extend([0; 4]);
        p.extend(payload);
        p
    }
    #[test]
    fn fu_a_and_loss() {
        let mut d = Depacketizer::default();
        assert!(d
            .push(&packet(1, false, &[0x7c, 0x85, 1, 2]))
            .unwrap()
            .is_none());
        let a = d
            .push(&packet(2, true, &[0x7c, 0x45, 3, 4]))
            .unwrap()
            .unwrap();
        assert!(a.key);
        assert_eq!(&a.data[4..], &[0x65, 1, 2, 3, 4]);
        let mut d = Depacketizer::default();
        d.push(&packet(1, false, &[0x7c, 0x85, 1])).unwrap();
        assert!(d
            .push(&packet(3, true, &[0x7c, 0x45, 2]))
            .unwrap()
            .is_none());
    }
    #[test]
    fn rejects_bad_rtp() {
        assert!(Depacketizer::default().push(&[0; 4]).is_err());
    }
}
