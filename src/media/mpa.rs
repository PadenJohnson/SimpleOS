//! MPEG audio layers 1, 2 and 3 (MP2 and MP3): in AVI and MKV movies, MPEG streams from
//! DVDs and TV, and old MP4s. Works on any pieces of the stream (frames may be split
//! between packets) and gives interleaved stereo.

use alloc::vec;
use alloc::vec::Vec;

use super::dec::AudioDec;
use super::mpatab::*;
use crate::video::bits::Bits;

const BITRATES: [[[u16; 15]; 3]; 2] = [
    // MPEG-1: layer 1, 2, 3
    [
        [0, 32, 64, 96, 128, 160, 192, 224, 256, 288, 320, 352, 384, 416, 448],
        [0, 32, 48, 56, 64, 80, 96, 112, 128, 160, 192, 224, 256, 320, 384],
        [0, 32, 40, 48, 56, 64, 80, 96, 112, 128, 160, 192, 224, 256, 320],
    ],
    // MPEG-2 and 2.5
    [
        [0, 32, 48, 56, 64, 80, 96, 112, 128, 144, 160, 176, 192, 224, 256],
        [0, 8, 16, 24, 32, 40, 48, 56, 64, 80, 96, 112, 128, 144, 160],
        [0, 8, 16, 24, 32, 40, 48, 56, 64, 80, 96, 112, 128, 144, 160],
    ],
];
const RATES: [u32; 9] = [44100, 48000, 32000, 22050, 24000, 16000, 11025, 12000, 8000];

#[derive(Clone, Copy)]
struct Header {
    /// MPEG-2 or 2.5 ("low sampling frequency").
    lsf: bool,
    layer: u8,
    crc: bool,
    bitrate: u32,
    /// Index into RATES (and the band tables).
    sri: usize,
    padding: bool,
    mode: u8,
    mode_ext: u8,
    channels: usize,
    len: usize,
}

fn header(b: &[u8]) -> Option<Header> {
    if b.len() < 4 || b[0] != 0xFF || b[1] & 0xE0 != 0xE0 {
        return None;
    }
    let ver = (b[1] >> 3) & 3; // 0 = 2.5, 2 = 2, 3 = 1
    let layer = 4 - ((b[1] >> 1) & 3);
    let bri = (b[2] >> 4) as usize;
    let sr = ((b[2] >> 2) & 3) as usize;
    if ver == 1 || layer == 4 || bri == 0 || bri == 15 || sr == 3 {
        return None;
    }
    let lsf = ver != 3;
    let sri = sr + if ver == 3 { 0 } else if ver == 2 { 3 } else { 6 };
    let bitrate = BITRATES[lsf as usize][layer as usize - 1][bri] as u32 * 1000;
    let rate = RATES[sri];
    let padding = b[2] & 2 != 0;
    let mode = b[3] >> 6;
    let len = match layer {
        1 => (12 * bitrate / rate + padding as u32) * 4,
        3 if lsf => 72 * bitrate / rate + padding as u32,
        _ => 144 * bitrate / rate + padding as u32,
    } as usize;
    Some(Header { lsf, layer, crc: b[1] & 1 == 0, bitrate, sri, padding, mode, mode_ext: (b[3] >> 4) & 3, channels: if mode == 3 { 1 } else { 2 }, len })
}

/// A Huffman code tree: entries are child node numbers, or -(symbol + 1) for leaves.
struct Tree {
    nodes: Vec<[i32; 2]>,
}

impl Tree {
    fn new() -> Tree {
        Tree { nodes: vec![[0, 0]] }
    }
    fn insert(&mut self, code: u32, len: u32, sym: u8) {
        let mut n = 0;
        for k in (0..len).rev() {
            let b = ((code >> k) & 1) as usize;
            if k == 0 {
                self.nodes[n][b] = -(sym as i32 + 1);
            } else {
                if self.nodes[n][b] <= 0 {
                    self.nodes.push([0, 0]);
                    let next = self.nodes.len() as i32 - 1;
                    self.nodes[n][b] = next;
                }
                n = self.nodes[n][b] as usize;
            }
        }
    }
    /// Codes handed out in order from their lengths (as FFmpeg's tables are listed).
    fn from_lengths(lens: &[u8], syms: &[u8]) -> Tree {
        let mut t = Tree::new();
        let mut code: u64 = 0;
        for (&l, &s) in lens.iter().zip(syms) {
            let l = l as u32;
            t.insert((code >> (32 - l)) as u32, l, s);
            code += 1u64 << (32 - l);
        }
        t
    }
    fn decode(&self, b: &mut Bits) -> u8 {
        let mut n = 0usize;
        for _ in 0..24 {
            let v = self.nodes[n][b.bit() as usize];
            if v < 0 {
                return (-v - 1) as u8;
            }
            if v == 0 {
                return 0;
            }
            n = v as usize;
        }
        0
    }
}

/// Tables made once per decoder.
struct Tables {
    huff: Vec<Tree>,
    quad_a: Tree,
    /// i^(4/3).
    pow43: Vec<f32>,
    /// 2^(q/4) for q = -512..=127.
    pow2q: Vec<f32>,
    /// Long IMDCT cosines [36][18], and the windows for block types 0, 1, 3 [36] and short [12].
    cos36: Vec<f32>,
    cos12: Vec<f32>,
    win: [[f32; 36]; 4],
    win12: [f32; 12],
    /// Antialias butterflies.
    cs: [f32; 8],
    ca: [f32; 8],
    /// Synthesis window (512).
    d: Vec<f32>,
    /// Long scale factor band starts (in samples) per sample rate.
    long_start: [[u16; 23]; 9],
}

fn sinf(x: f32) -> f32 {
    libm::sinf(x)
}
fn cosf(x: f32) -> f32 {
    libm::cosf(x)
}
const PI: f32 = core::f32::consts::PI;

impl Tables {
    fn new() -> Tables {
        let mut huff = vec![Tree::new()]; // 0: none
        let mut at = 0;
        for &n in HUFF_SIZES_MINUS_ONE.iter() {
            let n = n as usize + 1;
            huff.push(Tree::from_lengths(&HUFF_LENS[at..at + n], &HUFF_SYMS[at..at + n]));
            at += n;
        }
        let mut quad_a = Tree::new();
        for s in 0..16 {
            quad_a.insert(QUAD_CODES[s] as u32, QUAD_BITS[s] as u32, s as u8);
        }
        let pow43 = (0..8207).map(|i| if i == 0 { 0.0 } else { libm::powf(i as f32, 4.0 / 3.0) }).collect();
        let pow2q = (-512..128).map(|q| libm::exp2f(q as f32 / 4.0)).collect();
        let mut cos36 = vec![0f32; 36 * 18];
        for i in 0..36 {
            for k in 0..18 {
                cos36[i * 18 + k] = cosf(PI / 72.0 * (2 * i + 1 + 18) as f32 * (2 * k + 1) as f32);
            }
        }
        let mut cos12 = vec![0f32; 12 * 6];
        for i in 0..12 {
            for k in 0..6 {
                cos12[i * 6 + k] = cosf(PI / 24.0 * (2 * i + 1 + 6) as f32 * (2 * k + 1) as f32);
            }
        }
        let mut win = [[0f32; 36]; 4];
        for i in 0..36 {
            let s36 = sinf(PI / 36.0 * (i as f32 + 0.5));
            win[0][i] = s36;
            win[1][i] = match i {
                0..=17 => s36,
                18..=23 => 1.0,
                24..=29 => sinf(PI / 12.0 * ((i - 18) as f32 + 0.5)),
                _ => 0.0,
            };
            win[3][i] = match i {
                0..=5 => 0.0,
                6..=11 => sinf(PI / 12.0 * ((i - 6) as f32 + 0.5)),
                12..=17 => 1.0,
                _ => s36,
            };
        }
        let mut win12 = [0f32; 12];
        for (i, w) in win12.iter_mut().enumerate() {
            *w = sinf(PI / 12.0 * (i as f32 + 0.5));
        }
        let c = [-0.6f32, -0.535, -0.33, -0.185, -0.095, -0.041, -0.0142, -0.0037];
        let (mut cs, mut ca) = ([0f32; 8], [0f32; 8]);
        for i in 0..8 {
            let s = libm::sqrtf(1.0 + c[i] * c[i]);
            cs[i] = 1.0 / s;
            ca[i] = c[i] / s;
        }
        let mut d = vec![0f32; 512];
        for i in 0..257 {
            let v = ENWINDOW[i] as f32 / 65536.0;
            d[i] = v;
            if i != 0 && i != 256 {
                d[512 - i] = if i % 64 != 0 { -v } else { v };
            }
        }
        let mut long_start = [[0u16; 23]; 9];
        for (r, row) in long_start.iter_mut().enumerate() {
            let mut k = 0u16;
            for b in 0..22 {
                row[b] = k;
                k += BAND_SIZE_LONG[r * 22 + b] as u16;
            }
            row[22] = k;
        }
        Tables { huff, quad_a, pow43, pow2q, cos36, cos12, win, win12, cs, ca, d, long_start }
    }
    fn gain(&self, q: i32) -> f32 {
        self.pow2q[(q + 512).clamp(0, 639) as usize]
    }
}

#[derive(Clone, Copy)]
struct Granule {
    part23: usize,
    big_values: usize,
    global_gain: i32,
    sfc: usize,
    block_type: usize,
    mixed: bool,
    table: [usize; 3],
    subblock_gain: [i32; 3],
    /// Ends of the Huffman regions, in pairs of values.
    region: [usize; 3],
    preflag: bool,
    sf_scale: bool,
    count1_table: bool,
    scfsi: u8,
    /// Scale factors: long bands first (long_end of them), then 3 per short band.
    sf: [u8; 40],
    long_end: usize,
    short_start: usize,
}

impl Default for Granule {
    fn default() -> Granule {
        Granule { part23: 0, big_values: 0, global_gain: 0, sfc: 0, block_type: 0, mixed: false, table: [0; 3], subblock_gain: [0; 3], region: [0; 3], preflag: false, sf_scale: false, count1_table: false, scfsi: 0, sf: [0; 40], long_end: 22, short_start: 13 }
    }
}

/// The polyphase synthesis of one channel.
struct Synth {
    v: Vec<f32>,
    off: usize,
}

impl Synth {
    fn new() -> Synth {
        Synth { v: vec![0.0; 1024], off: 0 }
    }
    /// 32 subband samples in, 32 sound samples out.
    fn run(&mut self, t: &Tables, s: &[f32; 32], out: &mut [f32]) {
        self.off = (self.off + 1024 - 64) & 1023;
        let mut x = *s;
        dct32(&mut x);
        // V[i] for i = 0..64 from the DCT (see the matrixing formula's symmetries).
        for i in 0..64 {
            let v = match i {
                0..=15 => x[i + 16],
                16 => 0.0,
                17..=47 => -x[48 - i],
                _ => -x[i - 48],
            };
            self.v[(self.off + i) & 1023] = v;
        }
        for j in 0..32 {
            let mut sum = 0f32;
            for i in 0..8 {
                sum += self.v[(self.off + i * 128 + j) & 1023] * t.d[i * 64 + j];
                sum += self.v[(self.off + i * 128 + 96 + j) & 1023] * t.d[i * 64 + 32 + j];
            }
            out[j] = sum;
        }
    }
}

/// X[k] = sum x[n] cos(pi (2n+1) k / 64), in place (Lee's fast algorithm).
fn dct32(x: &mut [f32; 32]) {
    fn dct(x: &mut [f32]) {
        let n = x.len();
        if n == 1 {
            return;
        }
        let h = n / 2;
        let mut a = [0f32; 16];
        let mut b = [0f32; 16];
        for i in 0..h {
            a[i] = x[i] + x[n - 1 - i];
            b[i] = (x[i] - x[n - 1 - i]) / (2.0 * libm::cosf(PI * (2 * i + 1) as f32 / (2 * n) as f32));
        }
        dct(&mut a[..h]);
        dct(&mut b[..h]);
        for i in 0..h {
            x[2 * i] = a[i];
            x[2 * i + 1] = b[i] + if i + 1 < h { b[i + 1] } else { 0.0 };
        }
    }
    dct(&mut x[..]);
}

pub struct MpaDec {
    t: alloc::boxed::Box<Tables>,
    buf: Vec<u8>,
    reservoir: Vec<u8>,
    rate: u32,
    overlap: [[[f32; 18]; 32]; 2],
    synth: [Synth; 2],
    locked: Option<(bool, u8, usize)>,
}

impl MpaDec {
    pub fn new() -> MpaDec {
        MpaDec { t: alloc::boxed::Box::new(Tables::new()), buf: Vec::new(), reservoir: Vec::new(), rate: 0, overlap: [[[0.0; 18]; 32]; 2], synth: [Synth::new(), Synth::new()], locked: None }
    }

    fn frame(&mut self, f: &[u8], h: &Header) -> Vec<i16> {
        let mut b = Bits::new(&f[4..]);
        if h.crc {
            b.read(16);
        }
        // Subband samples per channel: [slot][32].
        let slots = match h.layer {
            1 => 12,
            2 => 36,
            _ => {
                if h.lsf {
                    18
                } else {
                    36
                }
            }
        };
        let mut sb = vec![[[0f32; 32]; 2]; slots];
        match h.layer {
            1 => self.layer1(&mut b, h, &mut sb),
            2 => self.layer2(&mut b, h, &mut sb),
            _ => self.layer3(f, h, &mut sb),
        }
        let mut out = Vec::with_capacity(slots * 64);
        let mut pcm = [[0f32; 32]; 2];
        for s in &sb {
            for ch in 0..h.channels {
                let t = &self.t;
                self.synth[ch].run(t, &s[ch], &mut pcm[ch]);
            }
            for j in 0..32 {
                let l = pcm[0][j];
                let r = if h.channels > 1 { pcm[1][j] } else { l };
                out.push(to16(l));
                out.push(to16(r));
            }
        }
        out
    }

    // ---- layers 1 and 2 ----

    fn layer1(&mut self, b: &mut Bits, h: &Header, sb: &mut [[[f32; 32]; 2]]) {
        let bound = if h.mode == 1 { (h.mode_ext as usize + 1) * 4 } else { 32 };
        let mut alloc = [[0u32; 32]; 2];
        for i in 0..32 {
            for ch in 0..h.channels {
                if i < bound || ch == 0 {
                    alloc[ch][i] = b.read(4);
                } else {
                    alloc[ch][i] = alloc[0][i];
                }
            }
        }
        let mut scale = [[0f32; 32]; 2];
        for i in 0..32 {
            for ch in 0..h.channels {
                if alloc[ch][i] != 0 {
                    scale[ch][i] = sf_value(b.read(6));
                }
            }
        }
        for s in sb.iter_mut().take(12) {
            for i in 0..32 {
                for ch in 0..h.channels {
                    let a = alloc[ch][i];
                    if a == 0 {
                        continue;
                    }
                    if i >= bound && ch == 1 {
                        // Joint stereo: same sample, own scale factor.
                        s[1][i] = s[0][i] / scale[0][i].max(1e-30) * scale[1][i];
                        continue;
                    }
                    let bits = a + 1;
                    let steps = (1u32 << bits) - 1;
                    s[ch][i] = level(b.read(bits), steps) * scale[ch][i];
                }
            }
        }
    }

    fn layer2(&mut self, b: &mut Bits, h: &Header, sb: &mut [[[f32; 32]; 2]]) {
        let table = if h.lsf {
            4
        } else {
            let per = h.bitrate / 1000 / h.channels as u32;
            let rate = RATES[h.sri];
            if (rate == 48000 && per >= 56) || (56..=80).contains(&per) {
                0
            } else if rate != 48000 && per >= 96 {
                1
            } else if rate != 32000 && per <= 48 {
                2
            } else {
                3
            }
        };
        let alloc_table: &[u8] = match table {
            0 | 1 => &ALLOC1,
            2 | 3 => &ALLOC3,
            _ => &ALLOC4,
        };
        let sblimit = SBLIMIT[table] as usize;
        let bound = (if h.mode == 1 { (h.mode_ext as usize + 1) * 4 } else { sblimit }).min(sblimit);
        let mut alloc = [[0usize; 32]; 2];
        let mut row = [0usize; 32];
        let mut j = 0;
        for i in 0..sblimit {
            row[i] = j;
            let nbal = alloc_table[j] as u32;
            if i < bound {
                for ch in 0..h.channels {
                    alloc[ch][i] = b.read(nbal) as usize;
                }
            } else {
                let v = b.read(nbal) as usize;
                alloc[0][i] = v;
                alloc[1][i] = v;
            }
            j += 1 << nbal;
        }
        let mut scfsi = [[0u32; 32]; 2];
        for i in 0..sblimit {
            for ch in 0..h.channels {
                if alloc[ch][i] != 0 {
                    scfsi[ch][i] = b.read(2);
                }
            }
        }
        let mut scale = [[[0f32; 3]; 32]; 2];
        for i in 0..sblimit {
            for ch in 0..h.channels {
                if alloc[ch][i] == 0 {
                    continue;
                }
                let sf = match scfsi[ch][i] {
                    0 => [b.read(6), b.read(6), b.read(6)],
                    1 => {
                        let a = b.read(6);
                        let c = b.read(6);
                        [a, a, c]
                    }
                    2 => {
                        let a = b.read(6);
                        [a, a, a]
                    }
                    _ => {
                        let a = b.read(6);
                        let c = b.read(6);
                        [a, c, c]
                    }
                };
                for k in 0..3 {
                    scale[ch][i][k] = sf_value(sf[k]);
                }
            }
        }
        for gr in 0..12 {
            let part = gr / 4; // scale factor part (each covers 12 slots)
            for i in 0..sblimit {
                for ch in 0..h.channels {
                    let a = alloc[ch][i];
                    if a == 0 || (i >= bound && ch == 1) {
                        continue;
                    }
                    let q = alloc_table[row[i] + a] as usize;
                    let steps = QUANT_STEPS[q];
                    let bits = QUANT_BITS[q];
                    let mut v = [0u32; 3];
                    if bits < 0 {
                        // Three samples in one code.
                        let mut c = b.read((-bits) as u32);
                        for x in v.iter_mut() {
                            *x = c % steps;
                            c /= steps;
                        }
                    } else {
                        for x in v.iter_mut() {
                            *x = b.read(bits as u32);
                        }
                    }
                    for m in 0..3 {
                        let val = level(v[m], steps);
                        sb[gr * 3 + m][ch][i] = val * scale[ch][i][part];
                        if i >= bound && h.channels == 2 {
                            sb[gr * 3 + m][1][i] = val * scale[1][i][part];
                        }
                    }
                }
            }
        }
    }

    // ---- layer 3 ----

    fn layer3(&mut self, f: &[u8], h: &Header, sb: &mut [[[f32; 32]; 2]]) {
        let side_len = match (h.lsf, h.channels) {
            (true, 1) => 9,
            (true, _) => 17,
            (false, 1) => 17,
            _ => 32,
        };
        let start = 4 + if h.crc { 2 } else { 0 };
        if f.len() < start + side_len {
            return;
        }
        let mut b = Bits::new(&f[start..start + side_len]);
        let ngr = if h.lsf { 1 } else { 2 };
        let begin = if h.lsf { b.read(8) } else { b.read(9) } as usize;
        b.read(if h.lsf { h.channels as u32 } else if h.channels == 1 { 5 } else { 3 });
        let mut g = [[Granule::default(); 2]; 2];
        if !h.lsf {
            for ch in 0..h.channels {
                g[1][ch].scfsi = b.read(4) as u8;
            }
        }
        let t = &*self.t;
        for gr in 0..ngr {
            for ch in 0..h.channels {
                let x = &mut g[gr][ch];
                x.part23 = b.read(12) as usize;
                x.big_values = (b.read(9) as usize).min(288);
                x.global_gain = b.read(8) as i32;
                x.sfc = b.read(if h.lsf { 9 } else { 4 }) as usize;
                let (r0, r1);
                if b.flag() {
                    x.block_type = b.read(2) as usize;
                    x.mixed = b.flag();
                    x.table = [b.read(5) as usize, b.read(5) as usize, 0];
                    for k in 0..3 {
                        x.subblock_gain[k] = b.read(3) as i32;
                    }
                    let first = if x.block_type == 2 {
                        if h.sri != 8 { 36 } else { 72 }
                    } else if h.sri <= 2 {
                        36
                    } else if h.sri != 8 {
                        54
                    } else {
                        108
                    };
                    r0 = first / 2;
                    r1 = 288;
                } else {
                    x.block_type = 0;
                    x.table = [b.read(5) as usize, b.read(5) as usize, b.read(5) as usize];
                    let a1 = b.read(4) as usize;
                    let a2 = b.read(3) as usize;
                    r0 = t.long_start[h.sri][a1 + 1] as usize / 2;
                    r1 = t.long_start[h.sri][(a1 + a2 + 2).min(22)] as usize / 2;
                }
                x.region = [r0.min(x.big_values), r1.min(x.big_values), x.big_values];
                if x.block_type == 2 {
                    if x.mixed {
                        x.long_end = if h.sri <= 2 { 8 } else { 6 };
                        x.short_start = 3;
                    } else {
                        x.long_end = 0;
                        x.short_start = 0;
                    }
                } else {
                    x.long_end = 22;
                    x.short_start = 13;
                }
                x.preflag = if h.lsf { false } else { b.flag() };
                x.sf_scale = b.flag();
                x.count1_table = b.flag();
            }
        }
        // Main data: from the bit reservoir (earlier frames) on into this one.
        let main_here = &f[start + side_len..f.len().min(h.len)];
        if self.reservoir.len() < begin {
            // Just after a jump: the data this frame needs came before.
            self.reservoir.extend_from_slice(main_here);
            let keep = self.reservoir.len().saturating_sub(4096);
            self.reservoir.drain(..keep);
            return;
        }
        let mut main = self.reservoir[self.reservoir.len() - begin..].to_vec();
        main.extend_from_slice(main_here);
        self.reservoir.extend_from_slice(main_here);
        let keep = self.reservoir.len().saturating_sub(4096);
        self.reservoir.drain(..keep);

        let mut b = Bits::new(&main);
        let mut xr = [[0f32; 576]; 2];
        for gr in 0..ngr {
            for ch in 0..h.channels {
                let pos0 = b.pos;
                let prev_sf = g[0][ch].sf;
                let x = &mut g[gr][ch];
                read_scale_factors(&mut b, h, ch, x, &prev_sf);
                let end = pos0 + x.part23;
                let nz = huffman(t, &mut b, h, x, end, &mut xr[ch]);
                let _ = nz;
                b.pos = end;
            }
            if h.mode == 1 && h.channels == 2 {
                stereo(t, h, &g[gr][1], &mut xr);
            }
            for ch in 0..h.channels {
                let x = &g[gr][ch];
                reorder(h, x, &mut xr[ch]);
                antialias(t, x, &mut xr[ch]);
                let ov = &mut self.overlap[ch];
                imdct(t, x, &xr[ch], ov, |slot, s, v| sb[gr * 18 + slot][ch][s] = v);
            }
        }
    }
}

fn to16(v: f32) -> i16 {
    let s = v * 32768.0;
    if s >= 32767.0 {
        32767
    } else if s <= -32768.0 {
        -32768
    } else {
        s as i16
    }
}

/// Layer 1/2 scale factor: 2^(1 - i/3).
fn sf_value(i: u32) -> f32 {
    libm::exp2f(1.0 - i as f32 / 3.0)
}

/// A layer 1/2 sample code to its value in (-1, 1).
fn level(code: u32, steps: u32) -> f32 {
    (2 * code as i32 - (steps as i32 - 1)) as f32 / steps as f32
}

fn read_scale_factors(b: &mut Bits, h: &Header, ch: usize, x: &mut Granule, prev: &[u8; 40]) {
    x.sf = [0; 40];
    if !h.lsf {
        let s1 = SLEN[x.sfc] as u32;
        let s2 = SLEN[16 + x.sfc] as u32;
        if x.block_type == 2 {
            let n = if x.mixed { 17 } else { 18 };
            let mut j = 0;
            for _ in 0..n {
                x.sf[j] = b.read(s1) as u8;
                j += 1;
            }
            for _ in 0..18 {
                x.sf[j] = b.read(s2) as u8;
                j += 1;
            }
        } else {
            let mut j = 0;
            for k in 0..4 {
                let n = if k == 0 { 6 } else { 5 };
                if x.scfsi & (8 >> k) == 0 {
                    let s = if k < 2 { s1 } else { s2 };
                    for _ in 0..n {
                        x.sf[j] = b.read(s) as u8;
                        j += 1;
                    }
                } else {
                    for _ in 0..n {
                        x.sf[j] = prev[j];
                        j += 1;
                    }
                }
            }
        }
        return;
    }
    // MPEG-2: the lengths and counts come from scalefac_compress.
    let tindex = if x.block_type == 2 { if x.mixed { 2 } else { 1 } } else { 0 };
    let mut sf = x.sfc;
    let split = |sf: &mut usize, n: usize| -> u32 {
        if n == 0 {
            return 0;
        }
        let d = *sf % n;
        *sf /= n;
        d as u32
    };
    let mut slen = [0u32; 4];
    let t2;
    if h.mode_ext & 1 != 0 && ch == 1 {
        sf >>= 1;
        let (n1, n2, n3, base, idx) = if sf < 180 { (6, 6, 0, 0, 3) } else if sf < 244 { (4, 4, 0, 180, 4) } else { (3, 0, 0, 244, 5) };
        sf -= base;
        slen[3] = split(&mut sf, n3);
        slen[2] = split(&mut sf, n2);
        slen[1] = split(&mut sf, n1);
        slen[0] = sf as u32;
        t2 = idx;
    } else {
        let (n1, n2, n3, base, idx) = if sf < 400 { (5, 4, 4, 0, 0) } else if sf < 500 { (5, 4, 0, 400, 1) } else { (3, 0, 0, 500, 2) };
        sf -= base;
        slen[3] = split(&mut sf, n3);
        slen[2] = split(&mut sf, n2);
        slen[1] = split(&mut sf, n1);
        slen[0] = sf as u32;
        if idx == 2 {
            x.preflag = true;
        }
        t2 = idx;
    }
    let mut j = 0;
    for k in 0..4 {
        let n = LSF_NSF[t2 * 12 + tindex * 4 + k] as usize;
        for _ in 0..n {
            if j < 40 {
                x.sf[j] = b.read(slen[k]) as u8;
            }
            j += 1;
        }
    }
}

/// Requantisation exponents (quarter steps of 2) for every sample of a granule.
fn exponents(h: &Header, x: &Granule, out: &mut [i32; 576]) {
    let gain = x.global_gain - 210;
    let shift = if x.sf_scale { 2 } else { 1 };
    let mut p = 0;
    let pre = if x.preflag { &PRETAB[22..] } else { &PRETAB[..22] };
    for i in 0..x.long_end {
        let v = gain - (((x.sf[i] + pre[i]) as i32) << shift);
        for _ in 0..BAND_SIZE_LONG[h.sri * 22 + i] {
            if p < 576 {
                out[p] = v;
                p += 1;
            }
        }
    }
    if x.short_start < 13 {
        let mut k = x.long_end;
        for i in x.short_start..13 {
            let len = BAND_SIZE_SHORT[h.sri * 13 + i];
            for w in 0..3 {
                let v = gain - 8 * x.subblock_gain[w] - ((x.sf.get(k).copied().unwrap_or(0) as i32) << shift);
                k += 1;
                for _ in 0..len {
                    if p < 576 {
                        out[p] = v;
                        p += 1;
                    }
                }
            }
        }
    }
    while p < 576 {
        out[p] = gain;
        p += 1;
    }
}

/// Huffman-coded values, requantised into `xr`. Returns how many may be non-zero.
fn huffman(t: &Tables, b: &mut Bits, h: &Header, x: &Granule, end: usize, xr: &mut [f32; 576]) -> usize {
    let mut e = [0i32; 576];
    exponents(h, x, &mut e);
    *xr = [0.0; 576];
    let val = |v: u32, i: usize| -> f32 { t.pow43[v.min(8206) as usize] * t.gain(e[i]) };
    let mut i = 0usize;
    for r in 0..3 {
        // Region ends are cumulative, in pairs of values.
        let stop = (x.region[r] * 2).min(576);
        let k = x.table[r];
        let (tab, linbits) = (HUFF_DATA[k * 2] as usize, HUFF_DATA[k * 2 + 1] as u32);
        while i < stop {
            if b.pos >= end {
                break;
            }
            if tab == 0 {
                i += 2;
                continue;
            }
            let s = t.huff[tab].decode(b);
            let (mut vx, mut vy) = ((s >> 4) as u32, (s & 15) as u32);
            if vx == 15 && linbits > 0 {
                vx += b.read(linbits);
            }
            if vx != 0 {
                let v = val(vx, i);
                xr[i] = if b.flag() { -v } else { v };
            }
            if vy == 15 && linbits > 0 {
                vy += b.read(linbits);
            }
            if vy != 0 {
                let v = val(vy, i + 1);
                xr[i + 1] = if b.flag() { -v } else { v };
            }
            i += 2;
        }
    }
    // Count1 region: groups of four small values.
    while i + 4 <= 576 && b.pos < end {
        let at = b.pos;
        let s = if x.count1_table { 15 - b.read(4) as u8 } else { t.quad_a.decode(b) };
        if b.pos > end {
            b.pos = at;
            break;
        }
        for k in 0..4 {
            if s & (8 >> k) != 0 {
                let v = val(1, i + k);
                xr[i + k] = if b.flag() { -v } else { v };
            }
        }
        i += 4;
    }
    i.min(576)
}

/// Joint stereo: mid/side and intensity stereo (`r` = the right channel's granule).
fn stereo(t: &Tables, h: &Header, r: &Granule, xr: &mut [[f32; 576]; 2]) {
    let ms = h.mode_ext & 2 != 0;
    let is = h.mode_ext & 1 != 0;
    let ms_band = |xr: &mut [[f32; 576]; 2], a: usize, n: usize| {
        if ms {
            for j in a..(a + n).min(576) {
                let (m, s) = (xr[0][j], xr[1][j]);
                xr[0][j] = (m + s) * core::f32::consts::FRAC_1_SQRT_2;
                xr[1][j] = (m - s) * core::f32::consts::FRAC_1_SQRT_2;
            }
        }
    };
    if !is {
        ms_band(xr, 0, 576);
        return;
    }
    // Intensity stereo: above the last band where the right channel has anything.
    let ratios = |sf: usize| -> Option<(f32, f32)> {
        if !h.lsf {
            if sf >= 7 {
                return None;
            }
            if sf == 6 {
                return Some((1.0, 0.0));
            }
            let f = libm::tanf(sf as f32 * PI / 12.0);
            Some((f / (1.0 + f), 1.0 / (1.0 + f)))
        } else {
            if sf >= 16 {
                return None;
            }
            let j = (r.sfc & 1) as f32;
            let f = libm::exp2f(-(j + 1.0) * ((sf + 1) >> 1) as f32 / 4.0);
            Some(if sf & 1 == 1 { (f, 1.0) } else { (1.0, f) })
        }
    };
    let _ = t;
    // Work down from the top, band by band, like the right channel's data.
    let mut bands: Vec<(usize, usize, usize)> = Vec::new(); // (start, len, sf index)
    let mut p = 0;
    for i in 0..r.long_end {
        let len = BAND_SIZE_LONG[h.sri * 22 + i] as usize;
        bands.push((p, len, if i == 21 { 20 } else { i }));
        p += len;
    }
    let mut short_bands: Vec<(usize, usize, usize, usize)> = Vec::new(); // (start, len, sf index, window)
    if r.short_start < 13 {
        let mut k = r.long_end;
        for i in r.short_start..13 {
            let len = BAND_SIZE_SHORT[h.sri * 13 + i] as usize;
            for w in 0..3 {
                let idx = if i == 12 { k - 3 } else { k };
                short_bands.push((p, len, idx, w));
                p += len;
                k += 1;
            }
        }
    }
    let mut nz_short = [false; 3];
    for &(a, n, idx, w) in short_bands.iter().rev() {
        if !nz_short[w] && xr[1][a..(a + n).min(576)].iter().all(|&v| v == 0.0) {
            if let Some((l, rr)) = ratios(r.sf.get(idx).copied().unwrap_or(0) as usize) {
                for j in a..(a + n).min(576) {
                    let v = xr[0][j];
                    xr[0][j] = v * l;
                    xr[1][j] = v * rr;
                }
                continue;
            }
        } else {
            nz_short[w] = true;
        }
        ms_band(xr, a, n);
    }
    let mut nz = nz_short.iter().any(|&b| b);
    for &(a, n, idx) in bands.iter().rev() {
        if !nz && xr[1][a..(a + n).min(576)].iter().all(|&v| v == 0.0) {
            if let Some((l, rr)) = ratios(r.sf[idx] as usize) {
                for j in a..(a + n).min(576) {
                    let v = xr[0][j];
                    xr[0][j] = v * l;
                    xr[1][j] = v * rr;
                }
                continue;
            }
        } else {
            nz = true;
        }
        ms_band(xr, a, n);
    }
}

/// Short blocks: from band-by-band, window-by-window order to interleaved windows.
fn reorder(h: &Header, x: &Granule, xr: &mut [f32; 576]) {
    if x.block_type != 2 {
        return;
    }
    let mut p = if x.mixed { if h.sri != 8 { 36 } else { 72 } } else { 0 };
    let mut tmp = [0f32; 576];
    for i in x.short_start..13 {
        let len = BAND_SIZE_SHORT[h.sri * 13 + i] as usize;
        if p + 3 * len > 576 {
            break;
        }
        for j in 0..len {
            for w in 0..3 {
                tmp[3 * j + w] = xr[p + w * len + j];
            }
        }
        xr[p..p + 3 * len].copy_from_slice(&tmp[..3 * len]);
        p += 3 * len;
    }
}

fn antialias(t: &Tables, x: &Granule, xr: &mut [f32; 576]) {
    let n = if x.block_type == 2 {
        if !x.mixed {
            return;
        }
        1
    } else {
        31
    };
    for sb in 1..=n {
        let base = sb * 18;
        for i in 0..8 {
            let a = xr[base - 1 - i];
            let b = xr[base + i];
            xr[base - 1 - i] = a * t.cs[i] - b * t.ca[i];
            xr[base + i] = b * t.cs[i] + a * t.ca[i];
        }
    }
}

/// IMDCT, windowing and overlap-add for each of the 32 subbands; gives (slot, subband, value).
fn imdct(t: &Tables, x: &Granule, xr: &[f32; 576], ov: &mut [[f32; 18]; 32], mut put: impl FnMut(usize, usize, f32)) {
    for s in 0..32 {
        let src = &xr[s * 18..s * 18 + 18];
        let mut z = [0f32; 36];
        let short = x.block_type == 2 && !(x.mixed && s < 2);
        if src.iter().all(|&v| v == 0.0) {
            // Nothing new: just the overlap from before.
        } else if short {
            for w in 0..3 {
                for i in 0..12 {
                    let mut sum = 0f32;
                    for k in 0..6 {
                        sum += src[3 * k + w] * t.cos12[i * 6 + k];
                    }
                    z[6 + 6 * w + i] += sum * t.win12[i];
                }
            }
        } else {
            let bt = if x.mixed && s < 2 { 0 } else { x.block_type };
            let win = &t.win[bt];
            for i in 0..36 {
                let mut sum = 0f32;
                let row = &t.cos36[i * 18..i * 18 + 18];
                for k in 0..18 {
                    sum += src[k] * row[k];
                }
                z[i] = sum * win[i];
            }
        }
        for i in 0..18 {
            let mut v = z[i] + ov[s][i];
            if s & 1 == 1 && i & 1 == 1 {
                v = -v; // frequency inversion
            }
            put(i, s, v);
            ov[s][i] = z[18 + i];
        }
    }
}

impl AudioDec for MpaDec {
    fn decode(&mut self, data: &[u8]) -> Vec<i16> {
        self.buf.extend_from_slice(data);
        let mut out = Vec::new();
        let mut p = 0;
        while p + 4 <= self.buf.len() {
            let Some(h) = header(&self.buf[p..]) else {
                p += 1;
                continue;
            };
            // Once locked onto a stream, ignore false syncs of another kind.
            if let Some(l) = self.locked {
                if l != (h.lsf, h.layer, h.sri) {
                    p += 1;
                    continue;
                }
            }
            if h.len < 8 || p + h.len > self.buf.len() {
                break;
            }
            self.locked = Some((h.lsf, h.layer, h.sri));
            self.rate = RATES[h.sri];
            let f = self.buf[p..p + h.len].to_vec();
            out.extend(self.frame(&f, &h));
            p += h.len;
            let _ = h.padding;
        }
        self.buf.drain(..p);
        if self.buf.len() > 1 << 16 {
            self.buf.clear(); // not MPEG audio after all
        }
        out
    }
    fn rate(&self) -> u32 {
        self.rate
    }
    fn reset(&mut self) {
        self.buf.clear();
        self.reservoir.clear();
        self.overlap = [[[0.0; 18]; 32]; 2];
        self.synth = [Synth::new(), Synth::new()];
    }
}
