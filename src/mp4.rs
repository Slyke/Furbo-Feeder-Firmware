//! Small ISO BMFF muxer for the camera's hardware-encoded H.264/AAC.
//! One RTP access unit per fragment. No transcoding and no unbounded queues.
fn atom(name: &[u8; 4], data: Vec<u8>) -> Vec<u8> {
    let mut b = Vec::with_capacity(data.len() + 8);
    b.extend(((data.len() + 8) as u32).to_be_bytes());
    b.extend(name);
    b.extend(data);
    b
}
fn full(name: &[u8; 4], version: u8, flags: u32, data: Vec<u8>) -> Vec<u8> {
    let mut b = vec![
        version,
        (flags >> 16) as u8,
        (flags >> 8) as u8,
        flags as u8,
    ];
    b.extend(data);
    atom(name, b)
}
fn words(values: &[u32]) -> Vec<u8> {
    values.iter().flat_map(|v| v.to_be_bytes()).collect()
}
fn matrix() -> Vec<u8> {
    words(&[0x10000, 0, 0, 0, 0x10000, 0, 0, 0, 0x40000000])
}
fn descriptor(tag: u8, body: Vec<u8>) -> Vec<u8> {
    let mut lengths = vec![(body.len() & 127) as u8];
    let mut n = body.len() >> 7;
    while n > 0 {
        lengths.push(((n & 127) as u8) | 128);
        n >>= 7;
    }
    lengths.reverse();
    let mut b = vec![tag];
    b.extend(lengths);
    b.extend(body);
    b
}
fn avc(sps: &[u8], pps: &[u8], w: u16, h: u16) -> Vec<u8> {
    let mut avcc = vec![1, sps[1], sps[2], sps[3], 255, 225];
    avcc.extend((sps.len() as u16).to_be_bytes());
    avcc.extend(sps);
    avcc.push(1);
    avcc.extend((pps.len() as u16).to_be_bytes());
    avcc.extend(pps);
    let mut b = vec![0; 6];
    b.extend(1u16.to_be_bytes());
    b.extend([0; 16]);
    b.extend(w.to_be_bytes());
    b.extend(h.to_be_bytes());
    b.extend(words(&[0x480000, 0x480000, 0]));
    b.extend(1u16.to_be_bytes());
    b.extend([0; 32]);
    b.extend(24u16.to_be_bytes());
    b.extend(0xffffu16.to_be_bytes());
    b.extend(atom(b"avcC", avcc));
    atom(b"avc1", b)
}
fn aac(config: &[u8], rate: u32, channels: u16) -> Vec<u8> {
    let mut dec = vec![0x40, 0x15, 0, 0, 0];
    dec.extend(words(&[64000, 32000]));
    dec.extend(descriptor(5, config.to_vec()));
    let mut es = vec![0, 2, 0];
    es.extend(descriptor(4, dec));
    es.extend(descriptor(6, vec![2]));
    let esds = full(b"esds", 0, 0, descriptor(3, es));
    let mut b = vec![0; 6];
    b.extend(1u16.to_be_bytes());
    b.extend([0; 8]);
    b.extend(channels.to_be_bytes());
    b.extend(16u16.to_be_bytes());
    b.extend([0; 4]);
    b.extend((rate << 16).to_be_bytes());
    b.extend(esds);
    atom(b"mp4a", b)
}
fn track(id: u32, timescale: u32, sample: Vec<u8>, w: u16, h: u16, audio: bool) -> Vec<u8> {
    let mut tk = words(&[0, 0, id, 0, 0]);
    tk.extend([0; 8]);
    tk.extend([0; 4]);
    tk.extend((if audio { 0x100u16 } else { 0 }).to_be_bytes());
    tk.extend([0; 2]);
    tk.extend(matrix());
    tk.extend(words(&[(w as u32) << 16, (h as u32) << 16]));
    let mut trak = full(b"tkhd", 0, 7, tk);
    let mut mdhd = words(&[0, 0, timescale, 0]);
    mdhd.extend([0x55, 0xc4, 0, 0]);
    let mut mdia = full(b"mdhd", 0, 0, mdhd);
    let mut handler = words(&[0]);
    handler.extend(if audio { b"soun" } else { b"vide" });
    handler.extend([0; 12]);
    handler.extend(if audio {
        b"Sound\0".as_slice()
    } else {
        b"Video\0".as_slice()
    });
    mdia.extend(full(b"hdlr", 0, 0, handler));
    let mut minf = if audio {
        full(b"smhd", 0, 0, vec![0; 4])
    } else {
        full(b"vmhd", 0, 1, vec![0; 8])
    };
    let mut dref = words(&[1]);
    dref.extend(full(b"url ", 0, 1, vec![]));
    minf.extend(atom(b"dinf", full(b"dref", 0, 0, dref)));
    let mut stsd = words(&[1]);
    stsd.extend(sample);
    let mut stbl = full(b"stsd", 0, 0, stsd);
    stbl.extend(full(b"stts", 0, 0, words(&[0])));
    stbl.extend(full(b"stsc", 0, 0, words(&[0])));
    stbl.extend(full(b"stsz", 0, 0, words(&[0, 0])));
    stbl.extend(full(b"stco", 0, 0, words(&[0])));
    minf.extend(atom(b"stbl", stbl));
    mdia.extend(atom(b"minf", minf));
    trak.extend(atom(b"mdia", mdia));
    atom(b"trak", trak)
}
pub fn init(sps: &[u8], pps: &[u8], w: u16, h: u16, audio: Option<(&[u8], u32, u16)>) -> Vec<u8> {
    let mut b = atom(
        b"ftyp",
        [b"isom".as_slice(), &0u32.to_be_bytes(), b"isomiso6avc1mp41"].concat(),
    );
    let mut mvhd = words(&[0, 0, 1000, 0, 0x10000]);
    mvhd.extend([1, 0, 0, 0]);
    mvhd.extend([0; 8]);
    mvhd.extend(matrix());
    mvhd.extend([0; 24]);
    mvhd.extend(3u32.to_be_bytes());
    let mut moov = full(b"mvhd", 0, 0, mvhd);
    moov.extend(track(1, 90000, avc(sps, pps, w, h), w, h, false));
    let mut mvex = full(b"trex", 0, 0, words(&[1, 1, 6000, 0, 0]));
    if let Some((config, rate, channels)) = audio {
        moov.extend(track(2, rate, aac(config, rate, channels), 0, 0, true));
        mvex.extend(full(b"trex", 0, 0, words(&[2, 1, 1024, 0, 0])));
    }
    moov.extend(atom(b"mvex", mvex));
    b.extend(atom(b"moov", moov));
    b
}
pub fn fragment(
    sequence: u32,
    track: u32,
    timestamp: u64,
    duration: u32,
    key: bool,
    data: &[u8],
) -> Vec<u8> {
    fn moof(
        sequence: u32,
        track: u32,
        ts: u64,
        duration: u32,
        key: bool,
        size: u32,
        offset: u32,
    ) -> Vec<u8> {
        let mut traf = full(b"tfhd", 0, 0x020000, words(&[track]));
        traf.extend(full(b"tfdt", 1, 0, ts.to_be_bytes().to_vec()));
        traf.extend(full(
            b"trun",
            0,
            0x000701,
            words(&[
                1,
                offset,
                duration,
                size,
                if key { 0x02000000 } else { 0x01010000 },
            ]),
        ));
        let mut b = full(b"mfhd", 0, 0, words(&[sequence]));
        b.extend(atom(b"traf", traf));
        atom(b"moof", b)
    }
    let size = data.len() as u32;
    let offset = moof(sequence, track, timestamp, duration, key, size, 0).len() as u32 + 8;
    let mut b = moof(sequence, track, timestamp, duration, key, size, offset);
    b.extend(atom(b"mdat", data.to_vec()));
    b
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn fragment_offset() {
        let b = fragment(1, 1, 0, 6000, true, &[1, 2, 3]);
        let n = u32::from_be_bytes(b[..4].try_into().unwrap()) as usize;
        assert_eq!(&b[n + 4..n + 8], b"mdat");
        let i = b.windows(4).position(|w| w == b"trun").unwrap();
        let offset = u32::from_be_bytes(b[i + 12..i + 16].try_into().unwrap());
        assert_eq!(offset as usize, n + 8);
    }
}

#[derive(Clone)]
struct BoxInfo {
    start: usize,
    len: usize,
    kind: [u8; 4],
}
fn boxes(bytes: &[u8], start: usize, end: usize) -> anyhow::Result<Vec<BoxInfo>> {
    let mut result = vec![];
    let mut at = start;
    while at < end {
        anyhow::ensure!(
            at.checked_add(8).is_some_and(|v| v <= end),
            "truncated MP4 box"
        );
        let len = u32::from_be_bytes(bytes[at..at + 4].try_into()?) as usize;
        anyhow::ensure!(
            len >= 8 && at.checked_add(len).is_some_and(|v| v <= end),
            "invalid MP4 box length"
        );
        anyhow::ensure!(result.len() < 4096, "too many MP4 boxes");
        result.push(BoxInfo {
            start: at,
            len,
            kind: bytes[at + 4..at + 8].try_into()?,
        });
        at += len;
    }
    Ok(result)
}
fn number(bytes: &[u8], at: usize) -> u32 {
    u32::from_be_bytes(bytes[at..at + 4].try_into().expect("validated MP4 field"))
}
fn children(bytes: &[u8], parent: &BoxInfo) -> anyhow::Result<Vec<BoxInfo>> {
    boxes(bytes, parent.start + 8, parent.start + parent.len)
}
fn find_box(list: &[BoxInfo], name: &[u8; 4]) -> anyhow::Result<BoxInfo> {
    list.iter()
        .find(|item| &item.kind == name)
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("missing MP4 box"))
}
/// Add finite movie/track durations and a random-access fragment index for downloaded clips.
/// Live streaming keeps the original open-ended initialization segment.
pub fn finalize(mut bytes: Vec<u8>) -> anyhow::Result<Vec<u8>> {
    anyhow::ensure!(bytes.len() <= 8_388_608, "recording too large");
    let top = boxes(&bytes, 0, bytes.len())?;
    if top.iter().any(|item| &item.kind == b"mfra") {
        return Ok(bytes);
    }
    let moov = find_box(&top, b"moov")?;
    let movie = children(&bytes, &moov)?;
    let mvhd = find_box(&movie, b"mvhd")?;
    anyhow::ensure!(
        mvhd.len >= 28 && bytes[mvhd.start + 8] == 0,
        "unsupported movie header"
    );
    let movie_scale = number(&bytes, mvhd.start + 20);
    anyhow::ensure!(movie_scale > 0, "invalid movie timescale");
    struct Track {
        id: u32,
        scale: u32,
        header: BoxInfo,
        media: BoxInfo,
        end: u64,
        index: Vec<(u64, u64)>,
    }
    let mut tracks = vec![];
    for trak in movie.iter().filter(|b| &b.kind == b"trak") {
        let parts = children(&bytes, trak)?;
        let header = find_box(&parts, b"tkhd")?;
        let mdia = find_box(&parts, b"mdia")?;
        let media = find_box(&children(&bytes, &mdia)?, b"mdhd")?;
        anyhow::ensure!(
            header.len >= 32
                && media.len >= 28
                && bytes[header.start + 8] == 0
                && bytes[media.start + 8] == 0,
            "unsupported track header"
        );
        let scale = number(&bytes, media.start + 20);
        anyhow::ensure!(scale > 0, "invalid track timescale");
        tracks.push(Track {
            id: number(&bytes, header.start + 20),
            scale,
            header,
            media,
            end: 0,
            index: vec![],
        });
    }
    for moof in top.iter().filter(|b| &b.kind == b"moof") {
        for traf in children(&bytes, moof)?
            .iter()
            .filter(|b| &b.kind == b"traf")
        {
            let parts = children(&bytes, traf)?;
            let tfhd = find_box(&parts, b"tfhd")?;
            let tfdt = find_box(&parts, b"tfdt")?;
            let trun = find_box(&parts, b"trun")?;
            anyhow::ensure!(
                tfhd.len >= 16 && tfdt.len >= 20 && trun.len >= 32,
                "truncated fragment metadata"
            );
            anyhow::ensure!(
                bytes[tfdt.start + 8] == 1
                    && number(&bytes, trun.start + 8) & 0xffffff == 0x701
                    && number(&bytes, trun.start + 12) == 1,
                "unsupported fragment format"
            );
            let id = number(&bytes, tfhd.start + 12);
            let timestamp = u64::from_be_bytes(bytes[tfdt.start + 12..tfdt.start + 20].try_into()?);
            let duration = u64::from(number(&bytes, trun.start + 20));
            let key = number(&bytes, trun.start + 28) & 0x10000 == 0;
            let track = tracks
                .iter_mut()
                .find(|t| t.id == id)
                .ok_or_else(|| anyhow::anyhow!("unknown fragment track"))?;
            track.end = track.end.max(timestamp.saturating_add(duration));
            if key {
                track.index.push((timestamp, moof.start as u64));
            }
        }
    }
    let mut movie_duration = 0u64;
    let mut index = vec![];
    for track in tracks {
        let duration = track.end.saturating_mul(u64::from(movie_scale)) / u64::from(track.scale);
        movie_duration = movie_duration.max(duration);
        anyhow::ensure!(
            duration <= u64::from(u32::MAX) && track.end <= u64::from(u32::MAX),
            "recording duration overflow"
        );
        bytes[track.header.start + 28..track.header.start + 32]
            .copy_from_slice(&(duration as u32).to_be_bytes());
        bytes[track.media.start + 24..track.media.start + 28]
            .copy_from_slice(&(track.end as u32).to_be_bytes());
        let mut table = words(&[track.id, 0, track.index.len() as u32]);
        for (time, offset) in track.index {
            table.extend(time.to_be_bytes());
            table.extend(offset.to_be_bytes());
            table.extend([1, 1, 1]);
        }
        index.extend(full(b"tfra", 1, 0, table));
    }
    bytes[mvhd.start + 24..mvhd.start + 28].copy_from_slice(&(movie_duration as u32).to_be_bytes());
    let size = index.len() + 24;
    index.extend(full(b"mfro", 0, 0, words(&[size as u32])));
    bytes.extend(atom(b"mfra", index));
    Ok(bytes)
}
#[cfg(test)]
mod seek_tests {
    use super::*;
    #[test]
    fn recording_has_finite_duration_and_keyframe_index() {
        let mut clip = init(&[0x67, 0x4d, 0, 0x29], &[0x68, 0], 1920, 1080, None);
        let first = clip.len();
        clip.extend(fragment(1, 1, 0, 45000, true, &[0, 0, 0, 1, 0x65]));
        clip.extend(fragment(2, 1, 45000, 45000, false, &[0, 0, 0, 1, 0x41]));
        let clip = finalize(clip).unwrap();
        let top = boxes(&clip, 0, clip.len()).unwrap();
        let moov = find_box(&top, b"moov").unwrap();
        let mvhd = find_box(&children(&clip, &moov).unwrap(), b"mvhd").unwrap();
        assert_eq!(number(&clip, mvhd.start + 24), 1000);
        let mfra = find_box(&top, b"mfra").unwrap();
        let tfra = find_box(&children(&clip, &mfra).unwrap(), b"tfra").unwrap();
        assert_eq!(number(&clip, tfra.start + 20), 1);
        assert_eq!(
            u64::from_be_bytes(clip[tfra.start + 32..tfra.start + 40].try_into().unwrap()),
            first as u64
        );
        assert_eq!(finalize(clip.clone()).unwrap(), clip);
        assert!(finalize(vec![0; 24]).is_err());
    }
}
