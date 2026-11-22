//! Host tests for Solon's media engine (src/media), compared with FFmpeg's results.
//!
//!   mediatest info <file>                      container, codecs, packet counts and times
//!   mediatest video <file> <ref.yuv> [frames]  decode every picture, compare with FFmpeg's
//!                                              yuv420p frames (ffmpeg -i f -f rawvideo -pix_fmt yuv420p ref.yuv)
//!   mediatest audio <file> <ref.raw>           decode the sound at 48 kHz stereo and compare with
//!                                              ffmpeg -i f -f s16le -ar 48000 -ac 2 ref.raw
//!   mediatest speed <file>                     decoding speed

extern crate alloc;

macro_rules! kprintln {
    ($($t:tt)*) => { println!($($t)*) };
}

#[path = "../../../src/video/bits.rs"]
#[allow(dead_code)]
pub mod bits;
#[path = "../../../src/video/tables.rs"]
pub mod tables;
#[path = "../../../src/video/h264d.rs"]
#[allow(dead_code)]
pub mod h264d;
#[path = "../../../src/video/h264cavlc.rs"]
pub mod h264cavlc;
#[path = "../../../src/video/h264cabac.rs"]
#[allow(dead_code)]
pub mod h264cabac;
#[path = "../../../src/video/h264.rs"]
#[allow(dead_code)]
pub mod h264;
#[path = "../../../src/video/aac.rs"]
#[allow(dead_code)]
pub mod aac;
#[path = "../../../src/video/mp4src.rs"]
#[allow(dead_code)]
pub mod mp4src;
#[path = "../../../src/video/mp4read.rs"]
#[allow(dead_code)]
pub mod mp4read;
#[path = "../../../src/video/avi.rs"]
#[allow(dead_code)]
pub mod avi;
#[path = "../../../src/image.rs"]
#[allow(dead_code)]
pub mod image;
#[path = "../../../src/media/mod.rs"]
#[allow(dead_code)]
mod media;

/// The kernel's module layout, for `crate::video::...` paths.
mod video {
    pub use super::{aac, avi, bits, h264d, mp4read, mp4src};
}

use std::fs::File;
use std::io::{Read as _, Seek, SeekFrom};
use std::time::Instant;

fn reader(path: &str) -> (impl FnMut(u64, usize) -> Option<Vec<u8>>, u64) {
    let mut f = File::open(path).expect("can't open the file");
    let size = f.metadata().unwrap().len();
    (
        move |off: u64, len: usize| {
            f.seek(SeekFrom::Start(off)).ok()?;
            let mut buf = vec![0u8; len];
            let mut got = 0;
            while got < len {
                match f.read(&mut buf[got..]) {
                    Ok(0) => break,
                    Ok(n) => got += n,
                    Err(_) => return None,
                }
            }
            buf.truncate(got);
            Some(buf)
        },
        size,
    )
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    match args.get(1).map(|s| s.as_str()) {
        Some("info") => info(&args[2]),
        Some("video") => video(&args[2], &args[3], args.get(4).and_then(|s| s.parse().ok()).unwrap_or(usize::MAX)),
        Some("audio") => audio(&args[2], &args[3]),
        Some("speed") => speed(&args[2]),
        Some("seek") => seek(&args[2]),
        _ => eprintln!("usage: mediatest info|video|audio|speed <file> ..."),
    }
}

fn info(path: &str) {
    let (mut read, size) = reader(path);
    let mut d = match media::open_demux(&mut read, size) {
        Ok(d) => d,
        Err(e) => return println!("can't open: {}", e),
    };
    let i = d.info();
    println!("duration {} ms", i.duration_ms);
    if let Some(v) = &i.video {
        println!("video {} {}x{} rot {}", v.codec.name(), v.width, v.height, v.rotation);
    }
    if let Some(a) = &i.audio {
        println!("audio {} {} Hz {} ch", a.codec.name(), a.rate, a.channels);
    }
    let (mut n, mut keys, mut bytes, mut first, mut last) = (0, 0, 0usize, None, 0i64);
    while let Some(p) = d.next_video(&mut read) {
        n += 1;
        keys += p.key as usize;
        bytes += p.data.len();
        if first.is_none() {
            first = p.pts;
        }
        last = p.pts.unwrap_or(last);
        if n <= 5 {
            println!("  v pts {:?} key {} {} bytes", p.pts, p.key, p.data.len());
        }
    }
    println!("video packets {} keys {} bytes {} first {:?} last {}", n, keys, bytes, first, last);
    let (mut n, mut bytes, mut last) = (0, 0usize, 0i64);
    while let Some(p) = d.next_audio(&mut read) {
        n += 1;
        bytes += p.data.len();
        last = p.pts.unwrap_or(last);
        if n <= 3 {
            println!("  a pts {:?} {} bytes", p.pts, p.data.len());
        }
    }
    println!("audio packets {} bytes {} last {}", n, bytes, last);
}

/// Decode all pictures in order (as playing does) and compare with FFmpeg's.
fn video(path: &str, reference: &str, max: usize) {
    let (mut read, size) = reader(path);
    let mut d = media::open_demux(&mut read, size).expect("open");
    let vi = d.info().video.as_ref().expect("no video");
    let mut dec = match media::dec::video(vi) {
        Ok(v) => v,
        Err(e) => return println!("no decoder: {}", e),
    };
    let refdata = std::fs::read(reference).unwrap_or_default();
    let mut frames: Vec<(media::dec::Pic, i64)> = Vec::new();
    let mut pending = std::collections::BinaryHeap::new();
    let t0 = Instant::now();
    let mut out = Vec::new();
    loop {
        let p = d.next_video(&mut read);
        match &p {
            Some(p) => {
                if !p.data.is_empty() {
                    pending.push(std::cmp::Reverse(p.pts.unwrap_or(0)));
                    dec.decode(&p.data);
                }
            }
            None => dec.flush(),
        }
        while let Some(pic) = dec.pop() {
            let pts = pending.pop().map(|r| r.0).unwrap_or(0);
            frames.push((pic, pts));
        }
        for f in frames.drain(..) {
            out.push(f);
        }
        if p.is_none() || out.len() >= max {
            break;
        }
    }
    let secs = t0.elapsed().as_secs_f64();
    println!("decoded {} pictures in {:.2} s ({:.1} fps)", out.len(), secs, out.len() as f64 / secs.max(1e-9));
    let mut worst = f64::MAX;
    let mut sum = 0.0;
    let mut n = 0;
    for (k, (pic, _)) in out.iter().enumerate() {
        let media::dec::Pic::Yuv(f) = pic else { continue };
        let (w, h) = (f.width, f.height);
        let fs = w * h * 3 / 2;
        let Some(r) = refdata.get(k * fs..(k + 1) * fs) else { break };
        // Compare Y, U and V.
        let mut se = 0f64;
        for y in 0..h {
            for x in 0..w {
                let a = f.y[(f.crop_y + y) * f.stride + f.crop_x + x] as f64;
                let b = r[y * w + x] as f64;
                se += (a - b) * (a - b);
            }
        }
        let cs = f.stride / 2;
        for plane in 0..2 {
            let src = if plane == 0 { &f.u } else { &f.v };
            let base = w * h + plane * (w / 2) * (h / 2);
            for y in 0..h / 2 {
                for x in 0..w / 2 {
                    let a = src[(f.crop_y / 2 + y) * cs + f.crop_x / 2 + x] as f64;
                    let b = r[base + y * (w / 2) + x] as f64;
                    se += (a - b) * (a - b);
                }
            }
        }
        let mse = se / fs as f64;
        let psnr = if mse == 0.0 { 99.0 } else { 10.0 * (255.0 * 255.0 / mse).log10() };
        if psnr < worst {
            worst = psnr;
        }
        let limit = if std::env::var("MT_WHERE").is_ok() { 98.0 } else { 30.0 };
        if psnr < limit && n < 400 {
            println!("  frame {} PSNR {:.1}", k, psnr);
            // MT_WHERE=1: which macroblocks differ (luma), as a map: '.' same, '+' close, '#' wrong.
            if std::env::var("MT_WHERE").is_ok() && psnr < 70.0 {
                for my in 0..h.div_ceil(16) {
                    let mut line = String::new();
                    for mx in 0..w.div_ceil(16) {
                        let mut worst_px = 0f64;
                        for y in my * 16..(my * 16 + 16).min(h) {
                            for x in mx * 16..(mx * 16 + 16).min(w) {
                                let a = f.y[(f.crop_y + y) * f.stride + f.crop_x + x] as f64;
                                worst_px = worst_px.max((a - r[y * w + x] as f64).abs());
                            }
                        }
                        line.push(if worst_px == 0.0 { '.' } else if worst_px < 8.0 { '+' } else { '#' });
                    }
                    println!("    {}", line);
                }
            }
        }
        sum += psnr;
        n += 1;
    }
    if n > 0 {
        println!("compared {} frames: average PSNR {:.2} dB, worst {:.2} dB", n, sum / n as f64, worst);
    }
}

fn audio(path: &str, reference: &str) {
    let (mut read, size) = reader(path);
    let mut e = match media::Engine::open(&mut read, size) {
        Ok(e) => e,
        Err(err) => return println!("can't open: {}", err),
    };
    if let Some(p) = &e.sound_problem {
        return println!("sound: {}", p);
    }
    let refdata: Vec<i16> = std::fs::read(reference).unwrap_or_default().chunks_exact(2).map(|c| i16::from_le_bytes([c[0], c[1]])).collect();
    let total = refdata.len() / 2;
    let t0 = Instant::now();
    let mut ours = Vec::with_capacity(refdata.len());
    let mut pos = 0u64;
    while (pos as usize) < total {
        let n = 2048.min(total - pos as usize);
        ours.extend(e.sound(&mut read, pos, n));
        pos += n as u64;
    }
    println!("decoded {:.2} s of sound in {:.2} s", total as f64 / 48000.0, t0.elapsed().as_secs_f64());
    // Signal to noise, allowing a small time offset (resampling and decoder delay differ).
    let mut best = (f64::MIN, 0i64);
    let mut shifts: Vec<i64> = (-2400i64..=2400).step_by(8).collect();
    let mut refine = true;
    while let Some(shift) = shifts.pop() {
        if shifts.is_empty() && refine {
            refine = false;
            let b = best.1;
            shifts.extend(b - 8..=b + 8);
        }
        let (mut sig, mut noise) = (0f64, 0f64);
        for i in (4800..total.saturating_sub(4800)).step_by(7) {
            let j = i as i64 + shift;
            if j < 0 || j as usize >= total {
                continue;
            }
            for ch in 0..2 {
                let a = refdata[i * 2 + ch] as f64;
                let b = ours[j as usize * 2 + ch] as f64;
                sig += a * a;
                noise += (a - b) * (a - b);
            }
        }
        let snr = if noise == 0.0 { 99.0 } else { 10.0 * (sig / noise).log10() };
        if snr > best.0 {
            best = (snr, shift);
        }
    }
    println!("sound SNR {:.1} dB (best at an offset of {} samples)", best.0, best.1);
    let _ = std::fs::write(format!("{}.solon.raw", reference), ours.iter().flat_map(|s| s.to_le_bytes()).collect::<Vec<u8>>());
}

fn speed(path: &str) {
    let (mut read, size) = reader(path);
    let mut d = media::open_demux(&mut read, size).expect("open");
    let vi = d.info().video.as_ref().expect("no video");
    let mut dec = media::dec::video(vi).expect("decoder");
    let t0 = Instant::now();
    let mut n = 0;
    while let Some(p) = d.next_video(&mut read) {
        dec.decode(&p.data);
        while dec.pop().is_some() {
            n += 1;
        }
    }
    dec.flush();
    while dec.pop().is_some() {
        n += 1;
    }
    let s = t0.elapsed().as_secs_f64();
    println!("{} pictures in {:.2} s = {:.1} fps", n, s, n as f64 / s);
}

/// Jump around (as dragging the position does) and check the picture shown is the one
/// at or just before each time.
fn seek(path: &str) {
    let (mut read, size) = reader(path);
    let mut e = media::Engine::open(&mut read, size).expect("open");
    let dur = e.duration_ms;
    let times = [0, dur / 2, dur / 4, dur * 3 / 4, 1000, dur.saturating_sub(500), 40, dur / 3];
    for &t in &times {
        let t0 = Instant::now();
        let r = e.picture(&mut read, t, 64, 36, 2000, None);
        let ms = t0.elapsed().as_millis();
        match r {
            Some((id, _)) => {
                let shown = id / 1000;
                let ok = shown as i64 <= t as i64 + 1 && (t as i64 - shown as i64) < 100;
                println!("  at {:>6} ms: picture from {:>6} ms {} ({} ms)", t, shown, if ok { "ok" } else { "WRONG" }, ms);
            }
            None => println!("  at {:>6} ms: no picture", t),
        }
    }
}
