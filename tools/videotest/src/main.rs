//! Host test for Solon's video export code (src/video): encodes a short clip with moving,
//! still and zooming parts plus a chord, and writes it as MP4 next to reference frames
//! (BMP) and the raw sound (PCM) so Windows' own decoders can check it.
//! Usage: videotest <out-folder> [width height]

extern crate alloc;

#[path = "../../../src/video/h264.rs"]
#[allow(dead_code)]
mod h264;
#[path = "../../../src/video/flac.rs"]
#[allow(dead_code)]
mod flac;
#[path = "../../../src/video/avi.rs"]
#[allow(dead_code)]
mod avi;
#[path = "../../../src/image.rs"]
#[allow(dead_code)]
mod image;
#[path = "../../../src/video/mp4.rs"]
#[allow(dead_code)]
mod mp4;

use std::fs;
use std::path::Path;

fn frame(w: usize, h: usize, t: usize) -> Vec<u32> {
    let mut px = vec![0u32; w * h];
    for y in 0..h {
        for x in 0..w {
            // A "photo": smooth colour gradient with some fine stripes and a circle.
            let (fx, fy) = (x as f32 / w as f32, y as f32 / h as f32);
            let (zx, zy) = if t >= 120 {
                // Zoom in towards the middle.
                let z = 1.0 - (t - 120) as f32 * 0.004;
                (0.5 + (fx - 0.5) * z, 0.5 + (fy - 0.5) * z)
            } else {
                (fx, fy)
            };
            let mut r = (zx * 255.0) as i32;
            let mut g = (zy * 200.0 + 30.0) as i32;
            let mut b = ((1.0 - zx) * 180.0 + 40.0) as i32;
            if ((zx * 40.0) as i32) % 2 == 0 && zy > 0.7 {
                r -= 40;
                g -= 40;
                b -= 40;
            }
            let (dx, dy) = (zx - 0.3, (zy - 0.4) * h as f32 / w as f32 * 1.0);
            if dx * dx + dy * dy < 0.01 {
                r = 250;
                g = 220;
                b = 40;
            }
            px[y * w + x] = 0xFF00_0000 | (r.clamp(0, 255) as u32) << 16 | (g.clamp(0, 255) as u32) << 8 | b.clamp(0, 255) as u32;
        }
    }
    if t < 60 {
        // A white box sliding across.
        let bx = t * (w - 80) / 60;
        for y in h / 3..h / 3 + 60 {
            for x in bx..bx + 80 {
                px[y * w + x] = 0xFFF0_F0F0;
            }
        }
    } else if t < 120 {
        // Still picture with a dark title bar fading in.
        let a = ((t - 60) * 4).min(160) as u32;
        for y in h - 70..h - 20 {
            for x in 40..w - 40 {
                let p = px[y * w + x];
                let mix = |c: u32| c * (255 - a) / 255;
                px[y * w + x] = 0xFF00_0000 | mix((p >> 16) & 0xFF) << 16 | mix((p >> 8) & 0xFF) << 8 | mix(p & 0xFF);
            }
        }
    }
    px
}

fn bmp(path: &Path, px: &[u32], w: usize, h: usize) {
    let row = (w * 3 + 3) & !3;
    let size = 54 + row * h;
    let mut b = Vec::with_capacity(size);
    b.extend_from_slice(b"BM");
    b.extend_from_slice(&(size as u32).to_le_bytes());
    b.extend_from_slice(&[0; 4]);
    b.extend_from_slice(&54u32.to_le_bytes());
    b.extend_from_slice(&40u32.to_le_bytes());
    b.extend_from_slice(&(w as i32).to_le_bytes());
    b.extend_from_slice(&(h as i32).to_le_bytes());
    b.extend_from_slice(&1u16.to_le_bytes());
    b.extend_from_slice(&24u16.to_le_bytes());
    b.extend_from_slice(&[0; 24]);
    for y in (0..h).rev() {
        for x in 0..w {
            let p = px[y * w + x];
            b.extend_from_slice(&[p as u8, (p >> 8) as u8, (p >> 16) as u8]);
        }
        b.resize(b.len() + row - w * 3, 0);
    }
    fs::write(path, b).unwrap();
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let out = Path::new(args.get(1).map(|s| s.as_str()).unwrap_or("video-out"));
    let w: usize = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(640);
    let h: usize = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(360);
    fs::create_dir_all(out).unwrap();
    avi_test(out);
    let frames = 180;
    let fps = 30;

    // Sound: a C major chord with a slow left/right sway, 6 seconds at 48 kHz.
    let rate = 48000;
    let total = rate * frames / fps;
    let mut pcm = vec![0i16; total * 2];
    for i in 0..total {
        let t = i as f64 / rate as f64;
        let s = [261.63, 329.63, 392.0].iter().map(|f| (t * f * std::f64::consts::TAU).sin()).sum::<f64>() / 3.0;
        let env = (t * 4.0).min(1.0) * 0.5;
        let pan = (t * 0.7).sin() * 0.5 + 0.5;
        pcm[i * 2] = (s * env * (1.0 - pan) * 32000.0) as i16;
        pcm[i * 2 + 1] = (s * env * pan * 32000.0) as i16;
    }
    // A couple of silent and loud edges to exercise the encoder.
    for v in pcm[..4096].iter_mut() {
        *v = 0;
    }
    let mut fl = flac::Encoder::new(rate as u32);
    let mut audio_frames = Vec::new();
    for chunk in pcm.chunks(flac::BLOCK * 2) {
        audio_frames.push((fl.frame(chunk), (chunk.len() / 2) as u32));
    }
    let flac_bytes: usize = audio_frames.iter().map(|f| f.0.len()).sum();
    println!("flac: {} -> {} bytes ({:.0}%)", pcm.len() * 2, flac_bytes, flac_bytes as f64 * 100.0 / (pcm.len() * 2) as f64);
    // A plain .flac file as well (Windows plays these directly).
    let mut f = b"fLaC".to_vec();
    f.push(0x80);
    f.extend_from_slice(&[0, 0, 34]);
    f.extend_from_slice(&fl.streaminfo());
    for (a, _) in &audio_frames {
        f.extend_from_slice(a);
    }
    fs::write(out.join("sound.flac"), f).unwrap();
    let raw: Vec<u8> = pcm.iter().flat_map(|s| s.to_le_bytes()).collect();
    fs::write(out.join("sound.pcm"), raw).unwrap();

    let mut enc = h264::Encoder::new(w, h, 26);
    enc.key_interval = 90;
    let mut mux = mp4::Muxer::new(w as u32, h as u32, fps as u32 * 1000, 1000, enc.sps(), enc.pps(), Some(mp4::AudioFormat { rate: rate as u32, channels: 2, bits: 16, streaminfo: fl.streaminfo() }));
    let mut next_audio = 0;
    let mut audio_time = 0u64; // in sample frames
    let refs = [0usize, 30, 59, 75, 119, 150, 179];
    let start = std::time::Instant::now();
    let mut keys = 0;
    for t in 0..frames {
        let px = frame(w, h, t);
        let (nal, key) = enc.encode(&px, false);
        keys += key as usize;
        mux.add_video(&[&nal], key);
        // Interleave: keep the sound up to half a second ahead of the picture.
        let vt = (t + 1) as u64 * rate as u64 / fps as u64;
        while next_audio < audio_frames.len() && audio_time < vt + rate as u64 / 2 {
            let (a, n) = &audio_frames[next_audio];
            mux.add_audio(a, *n);
            audio_time += *n as u64;
            next_audio += 1;
        }
        if refs.contains(&t) {
            bmp(&out.join(format!("ref{:03}.bmp", t)), &px, w, h);
        }
    }
    while next_audio < audio_frames.len() {
        let (a, n) = &audio_frames[next_audio];
        mux.add_audio(a, *n);
        next_audio += 1;
    }
    let el = start.elapsed().as_secs_f64();
    let mut file = mux.header();
    file.extend_from_slice(mux.payload());
    fs::write(out.join("test.mp4"), &file).unwrap();
    // Video only, too.
    let mut enc2 = h264::Encoder::new(w, h, 26);
    let mut mux2 = mp4::Muxer::new(w as u32, h as u32, fps as u32 * 1000, 1000, enc2.sps(), enc2.pps(), None);
    for t in 0..frames {
        let (nal, key) = enc2.encode(&frame(w, h, t), false);
        mux2.add_video(&[&nal], key);
    }
    let mut file2 = mux2.header();
    file2.extend_from_slice(mux2.payload());
    fs::write(out.join("video-only.mp4"), &file2).unwrap();
    println!("h264: {} frames {}x{} in {:.2}s ({:.1} fps), {} key, {} bytes ({:.0} kbit/s)", frames, w, h, el, frames as f64 / el, keys, file.len(), file.len() as f64 * 8.0 / 6.0 / 1000.0);
}

// ---- AVI (Motion-JPEG) reader test ----------------------------------------------------

fn jpeg_of(px: &[u32], w: usize, h: usize) -> Vec<u8> {
    let bytes: Vec<u8> = px.iter().flat_map(|p| p.to_le_bytes()).collect();
    let mut out = Vec::new();
    let enc = jpeg_encoder::Encoder::new(&mut out, 85);
    enc.encode(&bytes, w as u16, h as u16, jpeg_encoder::ColorType::Bgra).unwrap();
    out
}

/// Split a JPEG into its DHT segment bodies and the JPEG without them.
fn strip_dht(j: &[u8]) -> (Vec<u8>, Vec<u8>) {
    let mut out = j[..2].to_vec();
    let mut tables = Vec::new();
    let mut p = 2;
    while p + 4 <= j.len() {
        let m = j[p + 1];
        let len = u16::from_be_bytes([j[p + 2], j[p + 3]]) as usize;
        if m == 0xC4 {
            tables.extend_from_slice(&j[p + 4..p + 2 + len]);
        } else {
            out.extend_from_slice(&j[p..p + 2 + len]);
        }
        if m == 0xDA {
            out.extend_from_slice(&j[p + 2 + len..]);
            break;
        }
        p += 2 + len;
    }
    (tables, out)
}

fn le32(v: &mut Vec<u8>, x: u32) {
    v.extend_from_slice(&x.to_le_bytes());
}
fn le16(v: &mut Vec<u8>, x: u16) {
    v.extend_from_slice(&x.to_le_bytes());
}
fn chunk(v: &mut Vec<u8>, id: &[u8; 4], body: &[u8]) {
    v.extend_from_slice(id);
    le32(v, body.len() as u32);
    v.extend_from_slice(body);
    if body.len() % 2 == 1 {
        v.push(0);
    }
}
fn list(v: &mut Vec<u8>, kind: &[u8; 4], body: &[u8]) {
    let mut b = kind.to_vec();
    b.extend_from_slice(body);
    chunk(v, b"LIST", &b);
}

fn write_avi(jpegs: &[Vec<u8>], w: usize, h: usize, fps: u32, pcm: &[i16]) -> Vec<u8> {
    let rate = 48000u32;
    let mut avih = Vec::new();
    for x in [1_000_000 / fps, 0, 0, 0x10, jpegs.len() as u32, 0, 2, 0, w as u32, h as u32, 0, 0, 0, 0] {
        le32(&mut avih, x);
    }
    let strh = |kind: &[u8; 4], handler: &[u8; 4], scale: u32, rate: u32, len: u32, ssize: u32| {
        let mut s = Vec::new();
        s.extend_from_slice(kind);
        s.extend_from_slice(handler);
        for x in [0, 0, 0, scale, rate, 0, len, 0, 0xFFFF_FFFF, ssize] {
            le32(&mut s, x);
        }
        for _ in 0..4 {
            le16(&mut s, 0);
        }
        s
    };
    let mut bih = Vec::new();
    for x in [40u32, w as u32, h as u32] {
        le32(&mut bih, x);
    }
    le16(&mut bih, 1);
    le16(&mut bih, 24);
    bih.extend_from_slice(b"MJPG");
    for x in [(w * h * 3) as u32, 0, 0, 0, 0] {
        le32(&mut bih, x);
    }
    let mut wfx = Vec::new();
    le16(&mut wfx, 1);
    le16(&mut wfx, 2);
    le32(&mut wfx, rate);
    le32(&mut wfx, rate * 4);
    le16(&mut wfx, 4);
    le16(&mut wfx, 16);
    let mut strl_v = Vec::new();
    chunk(&mut strl_v, b"strh", &strh(b"vids", b"MJPG", 1, fps, jpegs.len() as u32, 0));
    chunk(&mut strl_v, b"strf", &bih);
    let mut strl_a = Vec::new();
    chunk(&mut strl_a, b"strh", &strh(b"auds", &[0; 4], 1, rate, (pcm.len() / 2) as u32, 4));
    chunk(&mut strl_a, b"strf", &wfx);
    let mut hdrl = Vec::new();
    chunk(&mut hdrl, b"avih", &avih);
    list(&mut hdrl, b"strl", &strl_v);
    list(&mut hdrl, b"strl", &strl_a);
    // movi: each frame followed by its share of the sound.
    let mut movi = Vec::new();
    let mut idx = Vec::new();
    let per = (rate / fps) as usize * 2;
    for (i, j) in jpegs.iter().enumerate() {
        let off = movi.len() as u32 + 4;
        chunk(&mut movi, b"00dc", j);
        idx.extend_from_slice(b"00dc");
        le32(&mut idx, 0x10);
        le32(&mut idx, off);
        le32(&mut idx, j.len() as u32);
        let a = &pcm[(i * per).min(pcm.len())..((i + 1) * per).min(pcm.len())];
        if !a.is_empty() {
            let bytes: Vec<u8> = a.iter().flat_map(|s| s.to_le_bytes()).collect();
            let off = movi.len() as u32 + 4;
            chunk(&mut movi, b"01wb", &bytes);
            idx.extend_from_slice(b"01wb");
            le32(&mut idx, 0x10);
            le32(&mut idx, off);
            le32(&mut idx, bytes.len() as u32);
        }
    }
    let mut body = b"AVI ".to_vec();
    list(&mut body, b"hdrl", &hdrl);
    list(&mut body, b"movi", &movi);
    chunk(&mut body, b"idx1", &idx);
    let mut f = Vec::new();
    chunk(&mut f, b"RIFF", &body);
    f
}

fn avi_test(out: &Path) {
    let (w, h) = (320usize, 240usize);
    let fps = 15;
    let n = 45;
    // The standard tables must be exactly what a normal JPEG encoder writes.
    let sample = jpeg_of(&frame(w, h, 0), w, h);
    let (tables, _) = strip_dht(&sample);
    let mine = avi::standard_dht();
    let split = |b: &[u8]| -> Vec<Vec<u8>> {
        let mut v = Vec::new();
        let mut p = 0;
        while p + 17 <= b.len() {
            let len = 17 + b[p + 1..p + 17].iter().map(|&x| x as usize).sum::<usize>();
            v.push(b[p..(p + len).min(b.len())].to_vec());
            p += len;
        }
        v.sort();
        v
    };
    // jpeg-encoder may order the four tables differently; compare as sets.
    let same = split(&tables) == split(&mine[4..]);
    println!("avi: standard Huffman tables match the encoder: {} ({} vs {} bytes)", same, tables.len(), mine.len() - 4);
    let frames: Vec<Vec<u32>> = (0..n).map(|t| frame(w, h, t * 2)).collect();
    let jpegs: Vec<Vec<u8>> = frames.iter().map(|f| strip_dht(&jpeg_of(f, w, h)).1).collect();
    let total = 48000 * n / fps;
    let pcm: Vec<i16> = (0..total * 2).map(|i| (((i / 2) as f64 * 523.25 * std::f64::consts::TAU / 48000.0).sin() * 9000.0) as i16).collect();
    let file = write_avi(&jpegs, w, h, fps as u32, &pcm);
    fs::write(out.join("camera.avi"), &file).unwrap();
    // Read it back with Solon's reader.
    let mut read = |off: u64, len: usize| -> Option<Vec<u8>> { file.get(off as usize..(off as usize + len).min(file.len())).map(|s| s.to_vec()) };
    let a = avi::Avi::parse(&mut read, file.len() as u64).expect("parse");
    println!(
        "avi: {}x{} {} frames at {}/{} fps, {} ms, sound {:?}",
        a.width,
        a.height,
        a.frames.len(),
        a.rate,
        a.scale,
        a.duration_ms(),
        a.sound.as_ref().map(|s| (s.rate, s.channels, s.bits, s.frames()))
    );
    let mut worst = 99.0f64;
    for i in [0usize, 20, 44] {
        let j = a.frame_jpeg(&mut read, i).unwrap();
        let img = image::decode(&j).expect("decode");
        let src = &frames[i];
        let mut se = 0.0;
        for (p, q) in img.px.iter().zip(src.iter()) {
            for s in [0, 8, 16] {
                let d = ((p >> s) & 0xFF) as f64 - ((q >> s) & 0xFF) as f64;
                se += d * d;
            }
        }
        let psnr = 10.0 * (255.0f64 * 255.0 / (se / (w * h * 3) as f64)).log10();
        worst = worst.min(psnr);
    }
    let snd = a.sound.as_ref().unwrap();
    let got = snd.read_frames(&mut read, 12345, 1000);
    let ok = got.iter().enumerate().all(|(k, &s)| s == pcm[12345 * 2 + k]);
    println!("avi: decoded frames PSNR >= {:.1} dB; sound read back exactly: {}", worst, ok);
    if !same || worst < 30.0 || !ok {
        println!("AVI TEST FAILED");
        std::process::exit(1);
    }
}
