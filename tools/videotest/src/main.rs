//! Host test for Solon's video export code (src/video): encodes a short clip with moving,
//! still and zooming parts plus a chord, and writes it as MP4 next to reference frames
//! (BMP) and the raw sound (PCM) so Windows' own decoders can check it.
//! Usage: videotest <out-folder> [width height]

extern crate alloc;

macro_rules! kprintln {
    ($($t:tt)*) => { println!($($t)*) };
}

#[path = "../../../src/video/bits.rs"]
#[allow(dead_code)]
mod bits;
#[path = "../../../src/video/tables.rs"]
mod tables;
#[path = "../../../src/video/h264d.rs"]
#[allow(dead_code)]
mod h264d;
#[path = "../../../src/video/h264cavlc.rs"]
mod h264cavlc;
#[path = "../../../src/video/h264cabac.rs"]
#[allow(dead_code)]
mod h264cabac;
#[path = "../../../src/video/aac.rs"]
#[allow(dead_code)]
mod aac;
#[path = "../../../src/video/mp4src.rs"]
mod mp4src;
#[path = "../../../src/video/mp4read.rs"]
#[allow(dead_code)]
mod mp4read;

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
    let mut all = aac_test(out);
    all &= decode_own(w, h);
    all &= mp4src_test(out);
    for name in ["win-baseline.mp4", "win-main.mp4", "win-high.mp4", "hw-baseline.mp4", "hw-main.mp4", "hw-high.mp4"] {
        all &= decode_file(&out.join(name), 640, 360);
    }
    if std::env::var("CMPWIN").is_ok() {
        compare_windows(out, "hw1080-h.mp4", "win1080-f", &[0, 1, 2, 5, 30, 100]);
        return;
    }
    unsafe { h264d::CLOCK = Some(host_clock) };
    decode_file(&out.join("hw1080-h.mp4"), 1920, 1080);
    print_times("1080p");
    unsafe { h264d::CLOCK = None };
    println!("decoder tests: {}", if all { "PASSED" } else { "FAILED" });
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

// ---- H.264 decoder tests ----------------------------------------------------------------

fn psnr_rgb(a: &[u32], b: &[u32]) -> f64 {
    let mut se = 0.0f64;
    for (p, q) in a.iter().zip(b.iter()) {
        for s in [0, 8, 16] {
            let d = ((p >> s) & 0xFF) as f64 - ((q >> s) & 0xFF) as f64;
            se += d * d;
        }
    }
    let mse = se / (a.len() * 3) as f64;
    if mse == 0.0 {
        99.0
    } else {
        10.0 * (255.0 * 255.0 / mse).log10()
    }
}

/// Decode our own encoder's stream; every frame must match its reconstruction exactly.
fn decode_own(w: usize, h: usize) -> bool {
    let mut enc = h264::Encoder::new(w, h, 26);
    enc.key_interval = 50;
    let mut dec = h264d::Decoder::new();
    let mut ok = true;
    let mut frames = 0;
    for t in 0..120 {
        let px = frame(w, h, t);
        let (nal, _) = enc.encode(&px, false);
        let mut sample = Vec::new();
        if t == 0 {
            for n in [enc.sps(), enc.pps()] {
                sample.extend_from_slice(&(n.len() as u32).to_be_bytes());
                sample.extend_from_slice(&n);
            }
        }
        sample.extend_from_slice(&(nal.len() as u32).to_be_bytes());
        sample.extend_from_slice(&nal);
        dec.decode_sample(&sample).unwrap();
        while let Some(f) = dec.pop() {
            let r = enc.recon().unwrap();
            let same = (0..f.height).all(|y| f.y[y * f.stride..y * f.stride + f.width] == r.y[y * r.w..y * r.w + f.width]) && f.u[..r.u.len()] == r.u[..] && f.v[..r.v.len()] == r.v[..];
            if !same {
                let diff = (0..f.height * f.stride).filter(|&i| f.y[i] != r.y[i]).count();
                let first = (0..f.height * f.stride).find(|&i| f.y[i] != r.y[i]);
                println!("own: frame {} differs ({} luma samples, first at {:?})", frames, diff, first.map(|i| (i % f.stride, i / f.stride)));
                ok = false;
                break;
            }
            frames += 1;
        }
        if !ok {
            break;
        }
    }
    println!("own stream {}x{}: {} frames decoded, {}", w, h, frames, if ok { "bit-exact" } else { "MISMATCH" });
    ok && frames == 120
}

/// Decode an MP4 made by Windows' encoder from our test clip; compare with the source.
fn decode_file(path: &Path, w: usize, h: usize) -> bool {
    let Ok(file) = fs::read(path) else {
        println!("{}: missing", path.display());
        return true;
    };
    let mut read = |off: u64, len: usize| -> Option<Vec<u8>> { file.get(off as usize..(off as usize + len).min(file.len())).map(|s| s.to_vec()) };
    let movie = mp4read::Movie::parse(&mut read, file.len() as u64).expect("mp4");
    let v = movie.video.as_ref().expect("video track");
    let mp4read::Codec::H264 { avcc } = &v.codec else {
        println!("not H.264");
        return false;
    };
    let sps = h264d::parse_sps(&avcc[8..8 + u16::from_be_bytes([avcc[6], avcc[7]]) as usize]).map(|s| s.1);
    let (profile, refs) = sps.as_ref().map(|s| (s.profile, s.max_refs)).unwrap_or((0, 0));
    let mut dec = h264d::Decoder::new();
    dec.configure(avcc).unwrap();
    dec.no_deblock = std::env::var("NODEBLOCK").is_ok();
    let pps_cabac = avcc.len() > 20;
    let _ = pps_cabac;
    // Presentation times, sorted, for matching output order.
    let mut pts: Vec<i64> = v.samples.iter().map(|s| s.dts as i64 + s.cts as i64).collect();
    pts.sort();
    let start = std::time::Instant::now();
    let mut out = Vec::new();
    for s in &v.samples {
        let data = &file[s.offset as usize..s.offset as usize + s.size as usize];
        dec.decode_sample(data).unwrap();
        while let Some(f) = dec.pop() {
            out.push(f);
        }
    }
    dec.flush();
    while let Some(f) = dec.pop() {
        out.push(f);
    }
    let el = start.elapsed().as_secs_f64();
    let mut worst = 99.0f64;
    let mut sum = 0.0;
    let mut worst_at = 0;
    for (k, f) in out.iter().enumerate() {
        let t = (pts[k.min(pts.len() - 1)] - pts[0]) as f64 / v.timescale as f64;
        let src_t = (t * 30.0).round() as usize;
        if src_t >= 180 {
            continue;
        }
        let got = f.to_rgb(w, h);
        let want: Vec<u32> = frame(w, h, src_t).iter().map(|p| p & 0xFFFFFF).collect();
        let p = psnr_rgb(&got, &want);
        sum += p;
        if p < worst {
            worst = p;
            worst_at = k;
        }
        if k == 100 {
            bmp(&path.with_extension("f100.bmp"), &got.iter().map(|p| p | 0xFF000000).collect::<Vec<_>>(), w, h);
        }
    }
    println!(
        "{}: profile {} refs {} {} samples -> {} frames in {:.2}s ({:.0} fps); PSNR vs source avg {:.1} dB, worst {:.1} dB (frame {}); errors {}; stats {:?}",
        path.file_name().unwrap().to_string_lossy(),
        profile,
        refs,
        v.samples.len(),
        out.len(),
        el,
        out.len() as f64 / el,
        sum / out.len().max(1) as f64,
        worst,
        worst_at,
        dec.errors,
        dec.stats
    );
    out.len() == v.samples.len() && worst > 28.0 && dec.errors == 0
}

/// Compare our decode of `file` with Windows' frames saved as win1080-f<N>.png (luma, per MB).
fn compare_windows(dir: &Path, file: &str, prefix: &str, frames: &[usize]) {
    let Ok(data) = fs::read(dir.join(file)) else { return };
    let mut read = |off: u64, len: usize| -> Option<Vec<u8>> { data.get(off as usize..(off as usize + len).min(data.len())).map(|s| s.to_vec()) };
    let movie = mp4read::Movie::parse(&mut read, data.len() as u64).unwrap();
    let v = movie.video.unwrap();
    let mp4read::Codec::H264 { avcc } = &v.codec else { return };
    let mut dec = h264d::Decoder::new();
    dec.configure(avcc).unwrap();
    let mut out = Vec::new();
    for s in &v.samples {
        dec.decode_sample(&data[s.offset as usize..(s.offset + s.size as u64) as usize]).unwrap();
        while let Some(f) = dec.pop() {
            out.push(f);
        }
        if out.len() > *frames.iter().max().unwrap() {
            break;
        }
    }
    let luma = |p: u32| (299 * ((p >> 16) & 255) + 587 * ((p >> 8) & 255) + 114 * (p & 255)) as f64 / 1000.0;
    for &n in frames {
        let Ok(png) = fs::read(dir.join(format!("{}{}.png", prefix, n))) else { continue };
        let win = image::decode(&png).unwrap();
        let f = &out[n];
        if n == frames[0] { println!("bt709 {} full_range {}", f.bt709, f.full_range); }
        let mut f601 = h264d::Frame { width: f.width, height: f.height, y: f.y.clone(), u: f.u.clone(), v: f.v.clone(), stride: f.stride, crop_x: f.crop_x, crop_y: f.crop_y, bt709: !f.bt709, full_range: f.full_range, poc: f.poc };
        if std::env::var("FLIP").is_ok() { core::mem::swap(&mut f601.bt709, &mut f601.full_range); f601.full_range = f.full_range; f601.bt709 = !f.bt709; }
        let f = if std::env::var("FLIP").is_ok() { &f601 } else { f };
        let mine = f.to_rgb(win.w, win.h);
        let (w, h) = (win.w, win.h);
        let mut se = 0.0;
        let mbw = w.div_ceil(16);
        let mut mb_err = vec![0.0f64; mbw * h.div_ceil(16)];
        for y in 0..h {
            for x in 0..w {
                let d = luma(mine[y * w + x]) - luma(win.px[y * w + x]);
                se += d * d;
                mb_err[(y / 16) * mbw + x / 16] += d.abs();
            }
        }
        let psnr = 10.0 * (65025.0 / (se / (w * h) as f64)).log10();
        let bad: Vec<usize> = (0..mb_err.len()).filter(|&i| mb_err[i] / 256.0 > 6.0).collect();
        println!("frame {}: luma PSNR vs Windows {:.1} dB, {} MBs off by >6 on average, first {:?}", n, psnr, bad.len(), bad.first().map(|&i| (i % mbw, i / mbw)));
        if n == frames[0] || bad.len() > 0 && bad.len() < 2000 {
            // A small map: one character per 2x2 MBs.
            let mut s = String::new();
            for r in (0..h.div_ceil(16)).step_by(2) {
                for c in (0..mbw).step_by(2) {
                    let e = mb_err[r * mbw + c] / 256.0;
                    s.push(if e < 2.0 { '.' } else if e < 6.0 { ':' } else { '#' });
                }
                s.push('\n');
            }
            if !bad.is_empty() {
                println!("{}", s);
            }
        }
    }
}

static T0: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();
fn host_clock() -> u64 {
    T0.get_or_init(std::time::Instant::now).elapsed().as_nanos() as u64
}

fn print_times(label: &str) {
    let t = unsafe { *core::ptr::addr_of!(h264d::TIMES) };
    let names = ["parse", "motion", "mc", "recon", "deblock", "output"];
    let s: Vec<String> = (0..6).map(|k| format!("{} {:.0}ms", names[k], t[k] as f64 / 1e6)).collect();
    println!("  {}: {}", label, s.join(", "));
    unsafe { *core::ptr::addr_of_mut!(h264d::TIMES) = [0; 6] };
}

// ---- AAC decoder tests ------------------------------------------------------------------

fn aac_test(dir: &Path) -> bool {
    // 1. The fast inverse MDCT must match the textbook formula.
    let mut ok = true;
    for n in [256usize, 2048] {
        let input: Vec<f32> = (0..n / 2).map(|k| (((k * 7919) % 1000) as f32 - 500.0) * if k % 3 == 0 { 1.0 } else { 0.3 }).collect();
        let a = aac::imdct_reference(&input, n);
        let b = aac::imdct_fast(&input, n);
        let peak = a.iter().fold(0f32, |m, v| m.max(v.abs()));
        let err = a.iter().zip(&b).fold(0f32, |m, (x, y)| m.max((x - y).abs()));
        println!("imdct {}: max error {:.5} (peak {:.1})", n, err, peak);
        ok &= err < peak * 1e-3;
    }
    // 2. Decode the AAC track of a Windows-made MP4 and compare with Windows' own decode.
    let Ok(data) = fs::read(dir.join("win-high.mp4")) else { return ok };
    let mut read = |off: u64, len: usize| -> Option<Vec<u8>> { data.get(off as usize..(off as usize + len).min(data.len())).map(|s| s.to_vec()) };
    let movie = mp4read::Movie::parse(&mut read, data.len() as u64).unwrap();
    let a = movie.audio.unwrap();
    let mp4read::Codec::Aac { asc } = &a.codec else {
        println!("no AAC track");
        return false;
    };
    let mut dec = aac::Aac::new(asc).unwrap();
    let start = std::time::Instant::now();
    let mut pcm: Vec<i16> = Vec::new();
    let mut errors = 0;
    for s in &a.samples {
        match dec.decode(&data[s.offset as usize..(s.offset + s.size as u64) as usize]) {
            Ok(p) => pcm.extend_from_slice(&p),
            Err(e) => {
                errors += 1;
                if errors < 5 {
                    println!("aac: {}", e);
                }
                pcm.extend_from_slice(&[0; 2048]);
            }
        }
    }
    let el = start.elapsed().as_secs_f64();
    let wav = fs::read(dir.join("win-high.mp4.wav")).unwrap_or_default();
    // Find the data chunk.
    let mut p = 12;
    let mut win: Vec<i16> = Vec::new();
    while p + 8 <= wav.len() {
        let sz = u32::from_le_bytes([wav[p + 4], wav[p + 5], wav[p + 6], wav[p + 7]]) as usize;
        if &wav[p..p + 4] == b"data" {
            win = wav[p + 8..(p + 8 + sz).min(wav.len())].chunks_exact(2).map(|c| i16::from_le_bytes([c[0], c[1]])).collect();
            break;
        }
        p += 8 + sz;
    }
    // Windows trims the encoder delay; find the best alignment.
    let mut best = (f64::MIN, 0isize);
    for shift in (-4096isize..=4096).step_by(2) {
        let mut num = 0f64;
        let mut den = 0f64;
        for i in (20000..120000).step_by(2) {
            let j = i as isize + shift;
            if j < 0 || j as usize >= pcm.len() || i >= win.len() {
                continue;
            }
            let (x, y) = (win[i] as f64, pcm[j as usize] as f64);
            num += x * y;
            den += y * y;
        }
        let score = num / den.max(1.0).sqrt();
        if score > best.0 {
            best = (score, shift);
        }
    }
    let shift = best.1;
    let mut se = 0f64;
    let mut sig = 0f64;
    for i in 20000..win.len().min(pcm.len().saturating_sub(5000)) {
        let j = (i as isize + shift) as usize;
        let d = win[i] as f64 - pcm[j] as f64;
        se += d * d;
        sig += win[i] as f64 * win[i] as f64;
    }
    let snr = 10.0 * (sig / se.max(1.0)).log10();
    println!(
        "aac: {} Hz {} ch{}, {} frames in {:.3}s, {} errors; vs Windows: offset {} samples, SNR {:.1} dB",
        dec.rate,
        dec.channels,
        if dec.sbr { " (HE)" } else { "" },
        a.samples.len(),
        el,
        errors,
        shift / 2,
        snr
    );
    ok && errors == 0 && snr > 40.0
}

/// Jump around an MP4 (forwards, backwards) and check each picture against the source.
fn mp4src_test(dir: &Path) -> bool {
    let Ok(data) = fs::read(dir.join("win-main.mp4")) else { return true };
    let mut read = |off: u64, len: usize| -> Option<Vec<u8>> { data.get(off as usize..(off as usize + len).min(data.len())).map(|s| s.to_vec()) };
    let mut s = mp4src::Mp4Source::open(&mut read, data.len() as u64).unwrap();
    let mut ok = true;
    let mut report = Vec::new();
    for ms in [0u64, 3000, 1000, 5900, 2000, 2033, 4500, 100] {
        let f = s.frame_at(&mut read, ms, 100_000).unwrap();
        let got = mp4src::frame_rgb(f, 0, 640, 360);
        let t = ((ms * 30 + 500) / 1000) as usize;
        let want: Vec<u32> = frame(640, 360, t.min(179)).iter().map(|p| p & 0xFFFFFF).collect();
        let p = psnr_rgb(&got, &want);
        report.push(format!("{}ms {:.1}dB", ms, p));
        ok &= p > 30.0;
    }
    let snd = s.audio(&mut read, 48000, 4800);
    let peak = snd.iter().map(|v| v.unsigned_abs()).max().unwrap_or(0);
    println!("mp4src: {} {}x{}, {} ms; {}; sound peak {}", if ok { "ok" } else { "BAD" }, s.width, s.height, s.duration_ms, report.join(", "), peak);
    ok && peak > 1000
}
