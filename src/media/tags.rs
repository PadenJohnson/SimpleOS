//! What a song file says about itself: title, artist, album, year, track number, genre and
//! the cover picture. Reads ID3v2 (2.2, 2.3, 2.4) and ID3v1 tags (MP3, AAC, WAV, AIFF),
//! Vorbis comments (FLAC, Ogg Vorbis, Opus) with their pictures, MP4 metadata (M4A, from
//! iTunes and phones) and WAV LIST/INFO.

use alloc::string::String;
use alloc::vec::Vec;

use super::Read;

#[derive(Default, Clone)]
pub struct Tags {
    pub title: Option<String>,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub album_artist: Option<String>,
    pub year: Option<String>,
    pub track: Option<u32>,
    pub disc: Option<u32>,
    pub genre: Option<String>,
    pub comment: Option<String>,
    /// The cover picture: the file's bytes (JPEG or PNG).
    pub cover: Option<Vec<u8>>,
}

impl Tags {
    fn set(slot: &mut Option<String>, v: String) {
        let v = String::from(v.trim_matches(|c: char| c == '\0' || c.is_whitespace()));
        if !v.is_empty() && slot.is_none() {
            *slot = Some(v);
        }
    }
    fn number(slot: &mut Option<u32>, v: &str) {
        // "3", "3/12", "03"
        if slot.is_none() {
            *slot = v.trim().split('/').next().and_then(|n| n.trim().parse().ok()).filter(|&n| n > 0);
        }
    }
    /// A Vorbis comment ("TITLE=...") or the like.
    fn comment(&mut self, key: &str, v: &str) {
        let k = key.to_ascii_uppercase();
        let v = String::from(v);
        match k.as_str() {
            "TITLE" => Tags::set(&mut self.title, v),
            "ARTIST" => Tags::set(&mut self.artist, v),
            "ALBUM" => Tags::set(&mut self.album, v),
            "ALBUMARTIST" | "ALBUM ARTIST" => Tags::set(&mut self.album_artist, v),
            "DATE" | "YEAR" => Tags::set(&mut self.year, String::from(v.get(..4).unwrap_or(&v))),
            "TRACKNUMBER" => Tags::number(&mut self.track, &v),
            "DISCNUMBER" => Tags::number(&mut self.disc, &v),
            "GENRE" => Tags::set(&mut self.genre, v),
            "COMMENT" | "DESCRIPTION" => Tags::set(&mut self.comment, v),
            "METADATA_BLOCK_PICTURE" if self.cover.is_none() => {
                if let Some(b) = base64(&v) {
                    self.cover = flac_picture(&b);
                }
            }
            "COVERART" if self.cover.is_none() => self.cover = base64(&v),
            _ => {}
        }
    }
}

/// Read a song file's tags (the start, and the end for ID3v1).
pub fn read(read: &mut Read, size: u64) -> Tags {
    let mut t = Tags::default();
    let Some(head) = read(0, 64) else { return t };
    if head.starts_with(b"ID3") {
        let n = super::audiofile::id3_size(&head);
        if let Some(d) = read(0, n.min(16 << 20) as usize) {
            id3v2(&d, &mut t);
        }
        // FLAC behind an ID3 tag (it happens): its own comments too.
        if let Some(h) = read(n, 4) {
            if h == b"fLaC" {
                flac_meta(read, n, &mut t);
            }
        }
    } else if head.starts_with(b"fLaC") {
        flac_meta(read, 0, &mut t);
    } else if head.starts_with(b"OggS") {
        ogg_comments(read, size, &mut t);
    } else if head.len() >= 12 && &head[0..4] == b"RIFF" && &head[8..12] == b"WAVE" {
        riff_info(read, size, &mut t);
    } else if head.len() >= 12 && &head[0..4] == b"FORM" && (&head[8..12] == b"AIFF" || &head[8..12] == b"AIFC") {
        aiff_chunks(read, size, &mut t);
    } else if head.len() >= 8 && &head[4..8] == b"ftyp" {
        mp4_meta(read, size, &mut t);
    }
    // ID3v1 at the end (old MP3s), for whatever is still missing.
    if size >= 128 {
        if let Some(d) = read(size - 128, 128) {
            if d.starts_with(b"TAG") {
                id3v1(&d, &mut t);
            }
        }
    }
    t
}

// ---- text ---------------------------------------------------------------------------------------

fn latin1(d: &[u8]) -> String {
    d.iter().take_while(|&&b| b != 0).map(|&b| b as char).collect()
}

fn utf16(d: &[u8], big: bool) -> String {
    let units = d.chunks_exact(2).map(|c| if big { u16::from_be_bytes([c[0], c[1]]) } else { u16::from_le_bytes([c[0], c[1]]) }).take_while(|&u| u != 0);
    char::decode_utf16(units).map(|r| r.unwrap_or('\u{FFFD}')).collect()
}

/// ID3 text with its encoding byte first.
fn id3_text(d: &[u8]) -> String {
    let Some((&enc, rest)) = d.split_first() else { return String::new() };
    match enc {
        1 if rest.len() >= 2 => match (rest[0], rest[1]) {
            (0xFE, 0xFF) => utf16(&rest[2..], true),
            (0xFF, 0xFE) => utf16(&rest[2..], false),
            _ => utf16(rest, false),
        },
        2 => utf16(rest, true),
        3 => String::from_utf8_lossy(rest.split(|&b| b == 0).next().unwrap_or(&[])).into_owned(),
        _ => latin1(rest),
    }
}

/// Skip a string ended by a zero (one byte, or two for UTF-16) in encoding `enc`.
fn skip_string(d: &[u8], enc: u8) -> &[u8] {
    if enc == 1 || enc == 2 {
        let mut i = 0;
        while i + 1 < d.len() {
            if d[i] == 0 && d[i + 1] == 0 {
                return &d[i + 2..];
            }
            i += 2;
        }
        &[]
    } else {
        match d.iter().position(|&b| b == 0) {
            Some(i) => &d[i + 1..],
            None => &[],
        }
    }
}

const GENRES: [&str; 80] = [
    "Blues", "Classic Rock", "Country", "Dance", "Disco", "Funk", "Grunge", "Hip-Hop", "Jazz", "Metal", "New Age", "Oldies", "Other", "Pop", "R&B", "Rap", "Reggae", "Rock", "Techno", "Industrial", "Alternative", "Ska", "Death Metal", "Pranks", "Soundtrack", "Euro-Techno", "Ambient", "Trip-Hop", "Vocal", "Jazz+Funk", "Fusion", "Trance", "Classical", "Instrumental", "Acid", "House", "Game", "Sound Clip", "Gospel", "Noise", "Alternative Rock", "Bass", "Soul", "Punk", "Space", "Meditative", "Instrumental Pop", "Instrumental Rock", "Ethnic", "Gothic", "Darkwave", "Techno-Industrial", "Electronic", "Pop-Folk", "Eurodance", "Dream", "Southern Rock", "Comedy", "Cult", "Gangsta", "Top 40", "Christian Rap", "Pop/Funk", "Jungle", "Native American", "Cabaret", "New Wave", "Psychedelic", "Rave", "Showtunes", "Trailer", "Lo-Fi", "Tribal", "Acid Punk", "Acid Jazz", "Polka", "Retro", "Musical", "Rock & Roll", "Hard Rock",
];

/// "(13)" or "13" (old numbered genres) to a name.
fn genre_name(g: &str) -> String {
    let n = g.trim().trim_start_matches('(').split(')').next().unwrap_or("");
    match n.parse::<usize>() {
        Ok(i) if i < GENRES.len() => String::from(GENRES[i]),
        _ => String::from(g),
    }
}

// ---- ID3 ----------------------------------------------------------------------------------------

fn id3v2(d: &[u8], t: &mut Tags) {
    if d.len() < 10 {
        return;
    }
    let ver = d[3];
    let flags = d[5];
    let mut body: Vec<u8> = d[10..].to_vec();
    // Version 2.3 and earlier: unsynchronisation over the whole tag (FF 00 -> FF).
    if flags & 0x80 != 0 && ver < 4 {
        body = unsync(&body);
    }
    let mut p = 0;
    if flags & 0x40 != 0 && ver >= 3 && body.len() >= 4 {
        let n = if ver == 4 { syncsafe(&body[0..4]) } else { u32::from_be_bytes([body[0], body[1], body[2], body[3]]) as usize + 4 };
        p = n.min(body.len());
    }
    let (id_len, hdr) = if ver == 2 { (3, 6) } else { (4, 10) };
    while p + hdr <= body.len() {
        let id = &body[p..p + id_len];
        if id[0] == 0 {
            break;
        }
        let size = match ver {
            2 => (body[p + 3] as usize) << 16 | (body[p + 4] as usize) << 8 | body[p + 5] as usize,
            3 => u32::from_be_bytes([body[p + 4], body[p + 5], body[p + 6], body[p + 7]]) as usize,
            _ => syncsafe(&body[p + 4..p + 8]),
        };
        let fflags = if ver >= 3 { body[p + 9] } else { 0 };
        let start = p + hdr;
        let end = (start + size).min(body.len());
        let mut f: Vec<u8> = body[start..end].to_vec();
        p = start + size;
        if ver == 4 && fflags & 0x02 != 0 {
            f = unsync(&f);
        }
        if ver == 4 && fflags & 0x01 != 0 && f.len() >= 4 {
            f.drain(..4); // data length indicator
        }
        if (ver == 3 && fflags & 0xC0 != 0) || (ver == 4 && fflags & 0x0C != 0) {
            continue; // compressed or encrypted
        }
        let id = core::str::from_utf8(id).unwrap_or("");
        match id {
            "TIT2" | "TT2" => Tags::set(&mut t.title, id3_text(&f)),
            "TPE1" | "TP1" => Tags::set(&mut t.artist, id3_text(&f)),
            "TALB" | "TAL" => Tags::set(&mut t.album, id3_text(&f)),
            "TPE2" | "TP2" => Tags::set(&mut t.album_artist, id3_text(&f)),
            "TYER" | "TYE" | "TDRC" | "TDOR" => {
                let y = id3_text(&f);
                Tags::set(&mut t.year, String::from(y.get(..4).unwrap_or(&y)));
            }
            "TRCK" | "TRK" => Tags::number(&mut t.track, &id3_text(&f)),
            "TPOS" | "TPA" => Tags::number(&mut t.disc, &id3_text(&f)),
            "TCON" | "TCO" => Tags::set(&mut t.genre, genre_name(&id3_text(&f))),
            "COMM" | "COM" if f.len() > 4 => {
                // Encoding, language (3), short description, text.
                let enc = f[0];
                let rest = skip_string(&f[4..], enc);
                let mut e = alloc::vec![enc];
                e.extend_from_slice(rest);
                Tags::set(&mut t.comment, id3_text(&e));
            }
            "APIC" if t.cover.is_none() && f.len() > 4 => {
                // Encoding, MIME type, picture type, description, data.
                let enc = f[0];
                let rest = match f[1..].iter().position(|&b| b == 0) {
                    Some(i) => &f[1 + i + 1..],
                    None => continue,
                };
                if rest.is_empty() {
                    continue;
                }
                let data = skip_string(&rest[1..], enc);
                if !data.is_empty() {
                    t.cover = Some(data.to_vec());
                }
            }
            "PIC" if t.cover.is_none() && f.len() > 6 => {
                // Encoding, format (3), picture type, description, data.
                let data = skip_string(&f[5..], f[0]);
                if !data.is_empty() {
                    t.cover = Some(data.to_vec());
                }
            }
            _ => {}
        }
    }
}

fn syncsafe(b: &[u8]) -> usize {
    b.iter().fold(0usize, |a, &x| a << 7 | (x & 0x7F) as usize)
}

fn unsync(d: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(d.len());
    let mut i = 0;
    while i < d.len() {
        out.push(d[i]);
        if d[i] == 0xFF && d.get(i + 1) == Some(&0) {
            i += 1;
        }
        i += 1;
    }
    out
}

fn id3v1(d: &[u8], t: &mut Tags) {
    let field = |a: usize, b: usize| String::from(latin1(&d[a..b]).trim());
    Tags::set(&mut t.title, field(3, 33));
    Tags::set(&mut t.artist, field(33, 63));
    Tags::set(&mut t.album, field(63, 93));
    Tags::set(&mut t.year, field(93, 97));
    if d[125] == 0 && d[126] != 0 && t.track.is_none() {
        t.track = Some(d[126] as u32);
    }
    if (d[127] as usize) < GENRES.len() {
        Tags::set(&mut t.genre, String::from(GENRES[d[127] as usize]));
    }
}

// ---- Vorbis comments and FLAC ------------------------------------------------------------------

fn vorbis_comments(d: &[u8], t: &mut Tags) {
    let le = |o: usize| d.get(o..o + 4).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]) as usize);
    let Some(vlen) = le(0) else { return };
    let mut p = 4 + vlen;
    let Some(n) = le(p) else { return };
    p += 4;
    for _ in 0..n.min(10_000) {
        let Some(l) = le(p) else { return };
        p += 4;
        let Some(c) = d.get(p..p + l) else { return };
        p += l;
        let c = String::from_utf8_lossy(c);
        if let Some((k, v)) = c.split_once('=') {
            t.comment(k, v);
        }
    }
}

/// A FLAC PICTURE block (also what METADATA_BLOCK_PICTURE holds): the picture's bytes.
fn flac_picture(d: &[u8]) -> Option<Vec<u8>> {
    let be = |o: usize| d.get(o..o + 4).map(|b| u32::from_be_bytes([b[0], b[1], b[2], b[3]]) as usize);
    let mut p = 4;
    let mime = be(p)?;
    p += 4 + mime;
    let desc = be(p)?;
    p += 4 + desc + 16;
    let len = be(p)?;
    p += 4;
    d.get(p..p + len).map(|b| b.to_vec())
}

fn flac_meta(read: &mut Read, at: u64, t: &mut Tags) {
    let mut pos = at + 4;
    for _ in 0..64 {
        let Some(h) = read(pos, 4) else { return };
        if h.len() < 4 {
            return;
        }
        let last = h[0] & 0x80 != 0;
        let kind = h[0] & 0x7F;
        let len = (h[1] as u64) << 16 | (h[2] as u64) << 8 | h[3] as u64;
        if kind == 4 || (kind == 6 && t.cover.is_none()) {
            if let Some(d) = read(pos + 4, len as usize) {
                if kind == 4 {
                    vorbis_comments(&d, t);
                } else {
                    t.cover = flac_picture(&d);
                }
            }
        }
        pos += 4 + len;
        if last {
            return;
        }
    }
}

/// The comment packet of an Ogg file's first stream (Vorbis, Opus or FLAC).
fn ogg_comments(read: &mut Read, size: u64, t: &mut Tags) {
    let mut pos = 0u64;
    let mut serial = None;
    let mut packets: Vec<Vec<u8>> = Vec::new();
    let mut partial = Vec::new();
    for _ in 0..64 {
        if pos >= size {
            return;
        }
        let Some(h) = read(pos, 27) else { return };
        if h.len() < 27 || &h[0..4] != b"OggS" {
            return;
        }
        let s = u32::from_le_bytes([h[14], h[15], h[16], h[17]]);
        let n = h[26] as usize;
        let Some(lacing) = read(pos + 27, n) else { return };
        let body: usize = lacing.iter().map(|&l| l as usize).sum();
        let Some(data) = read(pos + 27 + n as u64, body) else { return };
        pos += 27 + n as u64 + body as u64;
        if *serial.get_or_insert(s) != s {
            continue;
        }
        let mut off = 0;
        for &l in &lacing {
            partial.extend_from_slice(&data[off..(off + l as usize).min(data.len())]);
            off += l as usize;
            if l < 255 {
                packets.push(core::mem::take(&mut partial));
            }
        }
        if packets.len() >= 2 {
            let c = &packets[1];
            if c.starts_with(b"\x03vorbis") {
                vorbis_comments(&c[7..], t);
            } else if c.starts_with(b"OpusTags") {
                vorbis_comments(&c[8..], t);
            } else if c.len() > 4 && c[0] & 0x7F == 4 {
                vorbis_comments(&c[4..], t); // Ogg FLAC: a VORBIS_COMMENT block
            }
            return;
        }
    }
}

fn base64(s: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(s.len() * 3 / 4);
    let (mut acc, mut bits) = (0u32, 0);
    for c in s.bytes() {
        let v = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' | b'-' => 62,
            b'/' | b'_' => 63,
            b'=' | b'\r' | b'\n' | b' ' => continue,
            _ => return None,
        } as u32;
        acc = acc << 6 | v;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
        }
    }
    Some(out)
}

// ---- WAV, AIFF ------------------------------------------------------------------------------------

fn riff_info(read: &mut Read, size: u64, t: &mut Tags) {
    let mut pos = 12;
    for _ in 0..64 {
        if pos + 8 > size {
            return;
        }
        let Some(h) = read(pos, 8) else { return };
        let len = u32::from_le_bytes([h[4], h[5], h[6], h[7]]) as u64;
        match &h[0..4] {
            b"LIST" => {
                if let Some(d) = read(pos + 8, len.min(1 << 20) as usize) {
                    if d.starts_with(b"INFO") {
                        let mut p = 4;
                        while p + 8 <= d.len() {
                            let n = u32::from_le_bytes([d[p + 4], d[p + 5], d[p + 6], d[p + 7]]) as usize;
                            let v = latin1(&d[p + 8..(p + 8 + n).min(d.len())]);
                            match &d[p..p + 4] {
                                b"INAM" => Tags::set(&mut t.title, v),
                                b"IART" => Tags::set(&mut t.artist, v),
                                b"IPRD" => Tags::set(&mut t.album, v),
                                b"ICRD" => Tags::set(&mut t.year, String::from(v.get(..4).unwrap_or(&v))),
                                b"IGNR" => Tags::set(&mut t.genre, v),
                                b"ICMT" => Tags::set(&mut t.comment, v),
                                b"ITRK" | b"IPRT" => Tags::number(&mut t.track, &v),
                                _ => {}
                            }
                            p += 8 + n + (n & 1);
                        }
                    }
                }
            }
            b"id3 " | b"ID3 " => {
                if let Some(d) = read(pos + 8, len.min(16 << 20) as usize) {
                    id3v2(&d, t);
                }
            }
            _ => {}
        }
        pos += 8 + len + (len & 1);
    }
}

fn aiff_chunks(read: &mut Read, size: u64, t: &mut Tags) {
    let mut pos = 12;
    for _ in 0..64 {
        if pos + 8 > size {
            return;
        }
        let Some(h) = read(pos, 8) else { return };
        let len = u32::from_be_bytes([h[4], h[5], h[6], h[7]]) as u64;
        match &h[0..4] {
            b"NAME" | b"AUTH" | b"ANNO" => {
                if let Some(d) = read(pos + 8, len.min(4096) as usize) {
                    let v = latin1(&d);
                    match &h[0..4] {
                        b"NAME" => Tags::set(&mut t.title, v),
                        b"AUTH" => Tags::set(&mut t.artist, v),
                        _ => Tags::set(&mut t.comment, v),
                    }
                }
            }
            b"ID3 " | b"id3 " => {
                if let Some(d) = read(pos + 8, len.min(16 << 20) as usize) {
                    id3v2(&d, t);
                }
            }
            _ => {}
        }
        pos += 8 + len + (len & 1);
    }
}

// ---- MP4 (M4A) --------------------------------------------------------------------------------------

/// The child boxes of a box's body: (type, body).
fn boxes(d: &[u8]) -> Vec<([u8; 4], &[u8])> {
    let mut out = Vec::new();
    let mut p = 0;
    while p + 8 <= d.len() {
        let mut size = u32::from_be_bytes([d[p], d[p + 1], d[p + 2], d[p + 3]]) as usize;
        let kind = [d[p + 4], d[p + 5], d[p + 6], d[p + 7]];
        let mut hdr = 8;
        if size == 1 && p + 16 <= d.len() {
            size = u64::from_be_bytes(d[p + 8..p + 16].try_into().unwrap()) as usize;
            hdr = 16;
        } else if size == 0 {
            size = d.len() - p;
        }
        if size < hdr || p + size > d.len() {
            break;
        }
        out.push((kind, &d[p + hdr..p + size]));
        p += size;
    }
    out
}

fn mp4_meta(read: &mut Read, size: u64, t: &mut Tags) {
    // Find "moov" at the top level (it may be after the sound).
    let mut pos = 0u64;
    let mut moov = None;
    for _ in 0..64 {
        if pos + 8 > size {
            break;
        }
        let Some(h) = read(pos, 16) else { break };
        let mut len = u32::from_be_bytes([h[0], h[1], h[2], h[3]]) as u64;
        if len == 1 && h.len() >= 16 {
            len = u64::from_be_bytes(h[8..16].try_into().unwrap());
        } else if len == 0 {
            len = size - pos;
        }
        if len < 8 {
            break;
        }
        if &h[4..8] == b"moov" {
            moov = read(pos + 8, (len - 8).min(64 << 20) as usize);
            break;
        }
        pos += len;
    }
    let Some(moov) = moov else { return };
    let Some(udta) = boxes(&moov).into_iter().find(|b| &b.0 == b"udta").map(|b| b.1) else { return };
    let Some(meta) = boxes(udta).into_iter().find(|b| &b.0 == b"meta").map(|b| b.1) else { return };
    if meta.len() < 4 {
        return;
    }
    let Some(ilst) = boxes(&meta[4..]).into_iter().find(|b| &b.0 == b"ilst").map(|b| b.1) else { return };
    for (kind, body) in boxes(ilst) {
        let Some(data) = boxes(body).into_iter().find(|b| &b.0 == b"data").map(|b| b.1) else { continue };
        if data.len() < 8 {
            continue;
        }
        let v = &data[8..];
        let text = || String::from_utf8_lossy(v).into_owned();
        match &kind {
            b"\xA9nam" => Tags::set(&mut t.title, text()),
            b"\xA9ART" => Tags::set(&mut t.artist, text()),
            b"\xA9alb" => Tags::set(&mut t.album, text()),
            b"aART" => Tags::set(&mut t.album_artist, text()),
            b"\xA9day" => {
                let y = text();
                Tags::set(&mut t.year, String::from(y.get(..4).unwrap_or(&y)));
            }
            b"\xA9gen" => Tags::set(&mut t.genre, text()),
            b"gnre" if v.len() >= 2 => {
                let g = u16::from_be_bytes([v[0], v[1]]) as usize;
                if g >= 1 && g <= GENRES.len() {
                    Tags::set(&mut t.genre, String::from(GENRES[g - 1]));
                }
            }
            b"\xA9cmt" => Tags::set(&mut t.comment, text()),
            b"trkn" if v.len() >= 4 => {
                let n = u16::from_be_bytes([v[2], v[3]]) as u32;
                if n > 0 && t.track.is_none() {
                    t.track = Some(n);
                }
            }
            b"disk" if v.len() >= 4 => {
                let n = u16::from_be_bytes([v[2], v[3]]) as u32;
                if n > 0 && t.disc.is_none() {
                    t.disc = Some(n);
                }
            }
            b"covr" if t.cover.is_none() && !v.is_empty() => t.cover = Some(v.to_vec()),
            _ => {}
        }
    }
}
