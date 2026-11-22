//! Vorbis sound (WebM, MKV and Ogg files). Follows the Vorbis I specification, checked
//! against FFmpeg: codebooks, floor type 1, residues 0/1/2, channel coupling, both block
//! sizes. Floor type 0 (very old encoders) isn't supported. More than two channels are
//! mixed down to stereo.

use alloc::vec;
use alloc::vec::Vec;

use super::dec::AudioDec;
use super::vorbistab::INVERSE_DB;
use crate::video::aac::Imdct;

/// Vorbis packs bits least significant first.
struct LeBits<'a> {
    d: &'a [u8],
    pos: usize,
}

impl<'a> LeBits<'a> {
    fn new(d: &'a [u8]) -> LeBits<'a> {
        LeBits { d, pos: 0 }
    }
    #[inline]
    fn peek(&self, n: u32) -> u32 {
        if n == 0 {
            return 0;
        }
        let b = self.pos >> 3;
        let mut v: u64 = 0;
        for k in 0..5 {
            v |= (*self.d.get(b + k).unwrap_or(&0) as u64) << (8 * k);
        }
        ((v >> (self.pos & 7)) & ((1u64 << n) - 1)) as u32
    }
    #[inline]
    fn read(&mut self, n: u32) -> u32 {
        if n > 24 {
            let lo = self.read(16);
            let hi = self.read(n - 16);
            return lo | (hi << 16);
        }
        let v = self.peek(n);
        self.pos += n as usize;
        v
    }
    fn flag(&mut self) -> bool {
        self.read(1) != 0
    }
    fn left(&self) -> isize {
        self.d.len() as isize * 8 - self.pos as isize
    }
}

fn ilog(x: u32) -> u32 {
    32 - x.leading_zeros()
}

fn float32(x: u32) -> f32 {
    let mant = (x & 0x1f_ffff) as f32;
    let exp = ((x & 0x7fe0_0000) >> 21) as i32 - 788;
    let v = mant * libm::powf(2.0, exp as f32);
    if x & 0x8000_0000 != 0 {
        -v
    } else {
        v
    }
}

// ---- Codebooks ----

#[derive(Clone, Copy)]
struct Entry {
    sym: i32,
    len: i8,
}

struct Codebook {
    dims: usize,
    /// Lookup tables: root of ROOT bits, subtables after (len < 0 = subtable).
    table: Vec<Entry>,
    /// Vector values (entries x dims), if the book has them.
    vectors: Vec<f32>,
}

const ROOT: u32 = 10;

impl Codebook {
    fn build(lens: &[u8]) -> Option<Vec<Entry>> {
        // Codewords as the specification assigns them (first bit read = bit 0).
        let mut codes = vec![0u32; lens.len()];
        let mut exit = [0u32; 33];
        let first = lens.iter().position(|&l| l != 0)?;
        if lens[first] > 32 {
            return None;
        }
        for i in 0..lens[first] as usize {
            exit[i + 1] = 1 << i;
        }
        for p in first + 1..lens.len() {
            let l = lens[p] as usize;
            if l == 0 {
                continue;
            }
            let mut i = l;
            while i > 0 && exit[i] == 0 {
                i -= 1;
            }
            if i == 0 {
                return None;
            }
            let code = exit[i];
            exit[i] = 0;
            for j in i + 1..=l {
                exit[j] = code + (1 << (j - 1));
            }
            codes[p] = code;
        }
        let list: Vec<(u32, u8, i32)> = (0..lens.len()).filter(|&i| lens[i] > 0).map(|i| (codes[i], lens[i], i as i32)).collect();
        let mut table = Vec::new();
        build_level(&mut table, &list, ROOT);
        Some(table)
    }

    #[inline]
    fn decode(&self, b: &mut LeBits) -> Option<usize> {
        let mut base = 0usize;
        let mut bits = ROOT;
        loop {
            let e = self.table[base + b.peek(bits) as usize];
            if e.len > 0 {
                b.pos += e.len as usize;
                return Some(e.sym as usize);
            }
            if e.len == 0 {
                return None;
            }
            b.pos += bits as usize;
            base = e.sym as usize;
            bits = (-e.len) as u32;
        }
    }
}

/// One table level for codes whose first-read bit is bit 0.
fn build_level(table: &mut Vec<Entry>, codes: &[(u32, u8, i32)], bits: u32) -> usize {
    let start = table.len();
    table.extend(core::iter::repeat_n(Entry { sym: 0, len: 0 }, 1 << bits));
    for &(code, len, sym) in codes {
        if (len as u32) <= bits {
            let step = 1usize << len;
            let mut i = code as usize;
            while i < (1 << bits) {
                table[start + i] = Entry { sym, len: len as i8 };
                i += step;
            }
        }
    }
    let mask = (1u32 << bits) - 1;
    let mut longs: Vec<(u32, u8, i32)> = codes.iter().copied().filter(|c| c.1 as u32 > bits).collect();
    longs.sort_by_key(|c| c.0 & mask);
    let mut i = 0;
    while i < longs.len() {
        let prefix = longs[i].0 & mask;
        let mut j = i;
        let mut longest = 0;
        while j < longs.len() && longs[j].0 & mask == prefix {
            longest = longest.max(longs[j].1 as u32 - bits);
            j += 1;
        }
        let sub = longest.min(8);
        let rest: Vec<(u32, u8, i32)> = longs[i..j].iter().map(|&(c, l, s)| (c >> bits, l - bits as u8, s)).collect();
        let at = build_level(table, &rest, sub);
        table[start + prefix as usize] = Entry { sym: at as i32, len: -(sub as i8) };
        i = j;
    }
    start
}

fn parse_codebook(b: &mut LeBits) -> Option<Codebook> {
    if b.read(24) != 0x564342 {
        return None;
    }
    let dims = b.read(16) as usize;
    let entries = b.read(24) as usize;
    if entries == 0 || dims == 0 {
        return None;
    }
    let mut lens = vec![0u8; entries];
    if b.flag() {
        // Ordered.
        let mut cur = 0;
        let mut len = b.read(5) + 1;
        while cur < entries {
            let n = b.read(ilog((entries - cur) as u32)) as usize;
            if cur + n > entries || len > 32 {
                return None;
            }
            for l in lens.iter_mut().skip(cur).take(n) {
                *l = len as u8;
            }
            cur += n;
            len += 1;
        }
    } else {
        let sparse = b.flag();
        for l in lens.iter_mut() {
            if !sparse || b.flag() {
                *l = (b.read(5) + 1) as u8;
            }
        }
    }
    let lookup = b.read(4);
    let mut vectors = Vec::new();
    if lookup == 1 || lookup == 2 {
        let min = float32(b.read(32));
        let delta = float32(b.read(32));
        let vbits = b.read(4) + 1;
        let seq = b.flag();
        let nvals = if lookup == 1 {
            // The largest r with r^dims <= entries.
            let mut r = (libm::powf(entries as f32, 1.0 / dims as f32)) as usize;
            while (r + 1).checked_pow(dims as u32).is_some_and(|p| p <= entries) {
                r += 1;
            }
            while r > 0 && r.checked_pow(dims as u32).is_none_or(|p| p > entries) {
                r -= 1;
            }
            r
        } else {
            entries * dims
        };
        let mult: Vec<u32> = (0..nvals).map(|_| b.read(vbits)).collect();
        vectors = vec![0f32; entries * dims];
        for e in 0..entries {
            let mut last = 0f32;
            let mut div = 1usize;
            for i in 0..dims {
                let off = if lookup == 1 { (e / div) % nvals.max(1) } else { e * dims + i };
                let v = mult.get(off).copied().unwrap_or(0) as f32 * delta + min + last;
                vectors[e * dims + i] = v;
                if seq {
                    last = v;
                }
                div *= nvals.max(1);
            }
        }
    } else if lookup != 0 {
        return None;
    }
    // A book with a single used entry of length 0 isn't valid; one used entry gets length 1.
    let used = lens.iter().filter(|&&l| l > 0).count();
    let table = if used == 1 {
        let e = lens.iter().position(|&l| l > 0)?;
        let mut t = vec![Entry { sym: e as i32, len: 1 }; 1 << ROOT];
        for (i, x) in t.iter_mut().enumerate() {
            if i & 1 == 1 {
                *x = Entry { sym: 0, len: 0 };
            }
        }
        t
    } else {
        Codebook::build(&lens)?
    };
    Some(Codebook { dims, table, vectors })
}

// ---- Setup ----

struct Floor1 {
    partition_class: Vec<usize>,
    class_dims: Vec<usize>,
    class_sub: Vec<u32>,
    class_master: Vec<usize>,
    sub_books: Vec<Vec<i32>>,
    mult: u32,
    /// X positions, and for each its low/high neighbours and the sorted order.
    x: Vec<u32>,
    low: Vec<usize>,
    high: Vec<usize>,
    sorted: Vec<usize>,
}

struct Residue {
    kind: u32,
    begin: usize,
    end: usize,
    psize: usize,
    classes: usize,
    classbook: usize,
    books: Vec<[i32; 8]>,
}

struct Mapping {
    coupling: Vec<(usize, usize)>,
    mux: Vec<usize>,
    floors: Vec<usize>,
    residues: Vec<usize>,
}

struct Mode {
    block: bool,
    mapping: usize,
}

pub struct VorbisDec {
    channels: usize,
    rate: u32,
    bs: [usize; 2],
    books: Vec<Codebook>,
    floors: Vec<Option<Floor1>>,
    residues: Vec<Residue>,
    mappings: Vec<Mapping>,
    modes: Vec<Mode>,
    ready: bool,
    imdct: [Option<Imdct>; 2],
    windows: [Vec<f32>; 2],
    saved: Vec<Vec<f32>>,
    prev: Option<bool>,
    first: bool,
}

impl VorbisDec {
    /// `headers`: the three setup packets in Xiph lacing (as in MKV/WebM CodecPrivate).
    pub fn new(headers: &[u8]) -> VorbisDec {
        let mut d = VorbisDec {
            channels: 0,
            rate: 44100,
            bs: [256, 2048],
            books: Vec::new(),
            floors: Vec::new(),
            residues: Vec::new(),
            mappings: Vec::new(),
            modes: Vec::new(),
            ready: false,
            imdct: [None, None],
            windows: [Vec::new(), Vec::new()],
            saved: Vec::new(),
            prev: None,
            first: true,
        };
        if let Some(packets) = xiph_unlace(headers) {
            for p in packets {
                d.header(p);
            }
        }
        d
    }

    fn header(&mut self, p: &[u8]) {
        if p.len() < 7 || &p[1..7] != b"vorbis" {
            return;
        }
        let mut b = LeBits::new(&p[7..]);
        match p[0] {
            1 => {
                b.read(32);
                self.channels = b.read(8) as usize;
                self.rate = b.read(32);
                b.read(32);
                b.read(32);
                b.read(32);
                let s = b.read(8);
                self.bs = [1 << (s & 15), 1 << (s >> 4)];
            }
            5 => {
                if self.setup(&mut b).is_some() && self.channels > 0 {
                    for k in 0..2 {
                        let n = self.bs[k];
                        self.imdct[k] = Some(Imdct::new(n));
                        // Rising half of the Vorbis power-complementary window.
                        self.windows[k] = (0..n / 2)
                            .map(|i| {
                                let s = libm::sinf((i as f32 + 0.5) / n as f32 * core::f32::consts::PI);
                                libm::sinf(core::f32::consts::FRAC_PI_2 * s * s)
                            })
                            .collect();
                    }
                    self.saved = vec![vec![0.0; self.bs[1] / 2]; self.channels];
                    self.ready = true;
                }
            }
            _ => {}
        }
    }

    fn setup(&mut self, b: &mut LeBits) -> Option<()> {
        let n = b.read(8) + 1;
        for _ in 0..n {
            let cb = parse_codebook(b)?;
            self.books.push(cb);
        }
        let t = b.read(6) + 1;
        for _ in 0..t {
            if b.read(16) != 0 {
                return None;
            }
        }
        let nf = b.read(6) + 1;
        for _ in 0..nf {
            let kind = b.read(16);
            if kind == 1 {
                self.floors.push(Some(self.floor1_setup(b)?));
            } else {
                // Floor 0: read past its setup; its packets give silence.
                b.read(8 + 16 + 16 + 6 + 8);
                let nb = b.read(4) + 1;
                b.read(8 * nb);
                self.floors.push(None);
            }
        }
        let nr = b.read(6) + 1;
        for _ in 0..nr {
            let kind = b.read(16);
            if kind > 2 {
                return None;
            }
            let begin = b.read(24) as usize;
            let end = b.read(24) as usize;
            let psize = b.read(24) as usize + 1;
            let classes = b.read(6) as usize + 1;
            let classbook = b.read(8) as usize;
            let mut cascade = vec![0u32; classes];
            for c in cascade.iter_mut() {
                let mut v = b.read(3);
                if b.flag() {
                    v |= b.read(5) << 3;
                }
                *c = v;
            }
            let mut books = vec![[-1i32; 8]; classes];
            for (c, bk) in books.iter_mut().enumerate() {
                for (k, slot) in bk.iter_mut().enumerate() {
                    if cascade[c] & (1 << k) != 0 {
                        *slot = b.read(8) as i32;
                    }
                }
            }
            if begin > end || classbook >= self.books.len() {
                return None;
            }
            self.residues.push(Residue { kind, begin, end, psize, classes, classbook, books });
        }
        let nm = b.read(6) + 1;
        for _ in 0..nm {
            if b.read(16) != 0 {
                return None;
            }
            let submaps = if b.flag() { b.read(4) as usize + 1 } else { 1 };
            let mut coupling = Vec::new();
            if b.flag() {
                let steps = b.read(8) + 1;
                let bits = ilog(self.channels as u32 - 1);
                for _ in 0..steps {
                    let m = b.read(bits) as usize;
                    let a = b.read(bits) as usize;
                    if m >= self.channels || a >= self.channels || m == a {
                        return None;
                    }
                    coupling.push((m, a));
                }
            }
            b.read(2);
            let mut mux = vec![0usize; self.channels];
            if submaps > 1 {
                for m in mux.iter_mut() {
                    *m = b.read(4) as usize;
                }
            }
            let mut floors = Vec::new();
            let mut residues = Vec::new();
            for _ in 0..submaps {
                b.read(8);
                floors.push(b.read(8) as usize);
                residues.push(b.read(8) as usize);
            }
            self.mappings.push(Mapping { coupling, mux, floors, residues });
        }
        let nmodes = b.read(6) + 1;
        for _ in 0..nmodes {
            let block = b.flag();
            b.read(32);
            let mapping = b.read(8) as usize;
            if mapping >= self.mappings.len() {
                return None;
            }
            self.modes.push(Mode { block, mapping });
        }
        Some(())
    }

    fn floor1_setup(&self, b: &mut LeBits) -> Option<Floor1> {
        let parts = b.read(5) as usize;
        let mut partition_class = Vec::new();
        let mut maxc = 0;
        for _ in 0..parts {
            let c = b.read(4) as usize;
            maxc = maxc.max(c);
            partition_class.push(c);
        }
        let n = if parts > 0 { maxc + 1 } else { 0 };
        let (mut class_dims, mut class_sub, mut class_master, mut sub_books) = (vec![0; n], vec![0; n], vec![0; n], vec![Vec::new(); n]);
        for c in 0..n {
            class_dims[c] = b.read(3) as usize + 1;
            class_sub[c] = b.read(2);
            if class_sub[c] > 0 {
                class_master[c] = b.read(8) as usize;
            }
            for _ in 0..(1 << class_sub[c]) {
                sub_books[c].push(b.read(8) as i32 - 1);
            }
        }
        let mult = b.read(2) + 1;
        let rbits = b.read(4);
        let mut x = vec![0u32, 1 << rbits];
        for &pc in &partition_class {
            for _ in 0..class_dims[pc] {
                x.push(b.read(rbits));
            }
        }
        let v = x.len();
        let mut low = vec![0; v];
        let mut high = vec![1; v];
        for i in 2..v {
            for j in 0..i {
                if x[j] < x[i] && x[j] > x[low[i]] {
                    low[i] = j;
                }
                if x[j] > x[i] && x[j] < x[high[i]] {
                    high[i] = j;
                }
            }
        }
        let mut sorted: Vec<usize> = (0..v).collect();
        sorted.sort_by_key(|&i| x[i]);
        Some(Floor1 { partition_class, class_dims, class_sub, class_master, sub_books, mult, x, low, high, sorted })
    }

    /// Decode floor 1 into `out` (n values); false = this channel is silent.
    fn floor1(&self, f: &Floor1, b: &mut LeBits, out: &mut [f32]) -> Option<bool> {
        if !b.flag() {
            return Some(false);
        }
        let range = [256u32, 128, 86, 64][f.mult as usize - 1];
        let rb = ilog(range - 1);
        let v = f.x.len();
        let mut y = vec![0i32; v];
        y[0] = b.read(rb) as i32;
        y[1] = b.read(rb) as i32;
        let mut off = 2;
        for &pc in &f.partition_class {
            let cdim = f.class_dims[pc];
            let cbits = f.class_sub[pc];
            let csub = (1u32 << cbits) - 1;
            let mut cval = 0u32;
            if cbits > 0 {
                cval = self.books.get(f.class_master[pc])?.decode(b)? as u32;
            }
            for j in 0..cdim {
                let book = f.sub_books[pc][(cval & csub) as usize];
                cval >>= cbits;
                y[off + j] = if book >= 0 { self.books.get(book as usize)?.decode(b)? as i32 } else { 0 };
            }
            off += cdim;
        }
        // Amplitudes from predictions.
        let mut fin = vec![0i32; v];
        let mut used = vec![false; v];
        fin[0] = y[0];
        fin[1] = y[1];
        used[0] = true;
        used[1] = true;
        for i in 2..v {
            let (lo, hi) = (f.low[i], f.high[i]);
            let dy = fin[hi] - fin[lo];
            let adx = (f.x[hi] - f.x[lo]) as i32;
            let err = dy.abs() * (f.x[i] - f.x[lo]) as i32;
            let o = err / adx.max(1);
            let pred = if dy < 0 { fin[lo] - o } else { fin[lo] + o };
            let val = y[i];
            let high = range as i32 - pred;
            let low = pred;
            let room = if high < low { high * 2 } else { low * 2 };
            if val != 0 {
                used[lo] = true;
                used[hi] = true;
                used[i] = true;
                fin[i] = if val >= room {
                    if high > low {
                        val - low + pred
                    } else {
                        pred - val + high - 1
                    }
                } else if val & 1 != 0 {
                    pred - (val + 1) / 2
                } else {
                    pred + val / 2
                };
            } else {
                fin[i] = pred;
            }
            fin[i] = fin[i].clamp(0, 65535);
        }
        // Render lines between the used points.
        let n = out.len();
        let m = f.mult as i32;
        let mut lx = 0usize;
        let mut ly = fin[0] * m;
        for &i in f.sorted.iter().skip(1) {
            if used[i] {
                let hx = f.x[i] as usize;
                let hy = fin[i] * m;
                line(lx, ly, hx, hy, out);
                lx = hx;
                ly = hy;
            }
            if lx >= n {
                break;
            }
        }
        if lx < n {
            let v = INVERSE_DB[ly.clamp(0, 255) as usize];
            for o in out.iter_mut().skip(lx) {
                *o = v;
            }
        }
        Some(true)
    }

    #[allow(clippy::too_many_arguments)]
    fn residue(&self, r: &Residue, b: &mut LeBits, ch: usize, skip: &[bool], vecs: &mut [Vec<f32>], vlen: usize) -> Option<()> {
        let book = self.books.get(r.classbook)?;
        let cpc = book.dims;
        let (eff_ch, total) = if r.kind == 2 { (1, vlen * ch) } else { (ch, vlen) };
        if r.kind == 2 && skip.iter().all(|&s| s) {
            return Some(());
        }
        let end = r.end.min(total);
        if end <= r.begin {
            return Some(());
        }
        let parts = (end - r.begin) / r.psize;
        let mut classifs = vec![0usize; eff_ch * parts];
        let maxpass = r.books.iter().flat_map(|b| (0..8).filter(move |&k| b[k] >= 0)).max().unwrap_or(0);
        let do_ch = |j: usize| if r.kind == 2 { true } else { !skip[j] };
        for pass in 0..=maxpass {
            let mut p = 0;
            while p < parts {
                if pass == 0 {
                    for j in 0..eff_ch {
                        if !do_ch(j) {
                            continue;
                        }
                        let mut t = book.decode(b)?;
                        for i in (0..cpc).rev() {
                            if p + i < parts {
                                classifs[j * parts + p + i] = t % r.classes;
                            }
                            t /= r.classes;
                        }
                    }
                }
                for _ in 0..cpc {
                    if p >= parts {
                        break;
                    }
                    for j in 0..eff_ch {
                        if !do_ch(j) {
                            continue;
                        }
                        let cls = classifs[j * parts + p];
                        let vb = r.books[cls][pass];
                        if vb < 0 {
                            continue;
                        }
                        let cb = self.books.get(vb as usize)?;
                        if cb.vectors.is_empty() {
                            continue;
                        }
                        let dim = cb.dims;
                        let start = r.begin + p * r.psize;
                        match r.kind {
                            0 => {
                                let step = r.psize / dim;
                                for k in 0..step {
                                    let e = cb.decode(b)?;
                                    for l in 0..dim {
                                        vecs[j][start + k + l * step] += cb.vectors[e * dim + l];
                                    }
                                }
                            }
                            1 => {
                                let mut o = start;
                                while o < start + r.psize {
                                    let e = cb.decode(b)?;
                                    for l in 0..dim {
                                        if o < vlen {
                                            vecs[j][o] += cb.vectors[e * dim + l];
                                        }
                                        o += 1;
                                    }
                                }
                            }
                            _ => {
                                let mut o = start;
                                while o < start + r.psize {
                                    let e = cb.decode(b)?;
                                    for l in 0..dim {
                                        let (c, pos) = (o % ch, o / ch);
                                        if pos < vlen {
                                            vecs[c][pos] += cb.vectors[e * dim + l];
                                        }
                                        o += 1;
                                    }
                                }
                            }
                        }
                        if b.left() < 0 {
                            return Some(());
                        }
                    }
                    p += 1;
                }
            }
        }
        Some(())
    }

    /// One audio packet; returns interleaved stereo.
    fn packet(&mut self, data: &[u8]) -> Option<Vec<i16>> {
        let mut b = LeBits::new(data);
        if b.flag() {
            return None;
        }
        let nm = self.modes.len();
        let mode = if nm > 1 { b.read(ilog(nm as u32 - 1)) as usize } else { 0 };
        let m = self.modes.get(mode)?;
        let block = m.block as usize;
        let n = self.bs[block];
        let vlen = n / 2;
        let (prev, next) = if m.block {
            let flags = b.read(2);
            (flags & 1 != 0, flags & 2 != 0)
        } else {
            (false, false)
        };
        let map = &self.mappings[m.mapping];
        let ch = self.channels;
        let mut floor = vec![vec![0f32; vlen]; ch];
        let mut silent = vec![false; ch];
        for c in 0..ch {
            let fi = map.floors[if map.floors.len() > 1 { map.mux[c] } else { 0 }];
            match self.floors.get(fi)? {
                Some(f) => silent[c] = !self.floor1(f, &mut b, &mut floor[c])?,
                None => silent[c] = true,
            }
        }
        for &(mg, an) in map.coupling.iter().rev() {
            if !(silent[mg] && silent[an]) {
                silent[mg] = false;
                silent[an] = false;
            }
        }
        let mut res = vec![vec![0f32; vlen]; ch];
        for s in 0..map.floors.len() {
            let chans: Vec<usize> = (0..ch).filter(|&c| map.floors.len() == 1 || map.mux[c] == s).collect();
            if chans.is_empty() {
                continue;
            }
            let skip: Vec<bool> = chans.iter().map(|&c| silent[c]).collect();
            let mut sub: Vec<Vec<f32>> = chans.iter().map(|_| vec![0f32; vlen]).collect();
            let r = self.residues.get(map.residues[s])?;
            self.residue(r, &mut b, chans.len(), &skip, &mut sub, vlen)?;
            for (k, &c) in chans.iter().enumerate() {
                res[c] = core::mem::take(&mut sub[k]);
            }
        }
        // Inverse coupling.
        for &(mg, an) in map.coupling.iter().rev() {
            for i in 0..vlen {
                let (m, a) = (res[mg][i], res[an][i]);
                let (nm, na) = if m > 0.0 {
                    if a > 0.0 {
                        (m, m - a)
                    } else {
                        (m + a, m)
                    }
                } else if a > 0.0 {
                    (m, m + a)
                } else {
                    (m - a, m)
                };
                res[mg][i] = nm;
                res[an][i] = na;
            }
        }
        // Spectrum, transform, window, overlap-add (output runs from the centre of the
        // previous window to the centre of this one).
        let imdct = self.imdct[block].as_ref()?;
        let bs0 = self.bs[0];
        let pn = self.bs[prev as usize];
        let out_len = (n + pn) / 4;
        let mut outs = vec![vec![0f32; out_len]; ch];
        let mut full = vec![0f32; n];
        let mut spec = vec![0f32; vlen];
        // Window halves: (start, ramp table) for the left and right sides.
        let (lw, ls) = if m.block && !prev { (&self.windows[0], n / 4 - bs0 / 4) } else { (&self.windows[block], 0) };
        let (rw, rs) = if m.block && !next { (&self.windows[0], 3 * n / 4 - bs0 / 4) } else { (&self.windows[block], n / 2) };
        for c in 0..ch {
            if silent[c] {
                full.iter_mut().for_each(|v| *v = 0.0);
            } else {
                for i in 0..vlen {
                    spec[i] = floor[c][i] * res[c][i];
                }
                imdct.run(&spec, &mut full);
                let sc = n as f32 / 2.0;
                for i in 0..n {
                    let w = if i < n / 2 {
                        if i < ls {
                            0.0
                        } else if i - ls < lw.len() {
                            lw[i - ls]
                        } else {
                            1.0
                        }
                    } else if i < rs {
                        1.0
                    } else if i - rs < rw.len() {
                        rw[rw.len() - 1 - (i - rs)]
                    } else {
                        0.0
                    };
                    full[i] *= w * sc;
                }
            }
            let saved = &mut self.saved[c];
            let o = &mut outs[c];
            let off = n / 4;
            for (k, v) in o.iter_mut().enumerate() {
                let t = if k < pn / 2 { saved[k] } else { 0.0 };
                let ci = k + off;
                *v = t + if ci >= pn / 4 && ci - pn / 4 < n / 2 { full[ci - pn / 4] } else { 0.0 };
            }
            saved[..n / 2].copy_from_slice(&full[n / 2..]);
        }
        self.prev = Some(m.block);
        if self.first {
            self.first = false;
            return Some(Vec::new());
        }
        // Mix to stereo.
        let mut pcm = Vec::with_capacity(out_len * 2);
        let s = |v: f32| -> i16 {
            let x = v * 32768.0;
            (if x >= 0.0 { x + 0.5 } else { x - 0.5 }).clamp(-32768.0, 32767.0) as i16
        };
        // Vorbis channel order, mixed to stereo (weights for left, right), normalised so the
        // loudest side sums to 1 (as FFmpeg mixes).
        const H: f32 = core::f32::consts::FRAC_1_SQRT_2;
        let mix: &[(f32, f32)] = match ch {
            1 => &[(1.0, 1.0)],
            2 => &[(1.0, 0.0), (0.0, 1.0)],
            3 => &[(1.0, 0.0), (H, H), (0.0, 1.0)],
            4 => &[(1.0, 0.0), (0.0, 1.0), (H, 0.0), (0.0, H)],
            5 => &[(1.0, 0.0), (H, H), (0.0, 1.0), (H, 0.0), (0.0, H)],
            6 => &[(1.0, 0.0), (H, H), (0.0, 1.0), (H, 0.0), (0.0, H), (0.0, 0.0)],
            7 => &[(1.0, 0.0), (H, H), (0.0, 1.0), (H, 0.0), (0.0, H), (0.5, 0.5), (0.0, 0.0)],
            _ => &[(1.0, 0.0), (H, H), (0.0, 1.0), (H, 0.0), (0.0, H), (H, 0.0), (0.0, H), (0.0, 0.0)],
        };
        let norm = 1.0 / mix.iter().map(|m| m.0).sum::<f32>();
        for i in 0..out_len {
            let (mut l, mut r) = (0f32, 0f32);
            for (c, &(wl, wr)) in mix.iter().enumerate().take(ch) {
                l += outs[c][i] * wl;
                r += outs[c][i] * wr;
            }
            let (l, r) = if ch == 1 { (l, r) } else { (l * norm, r * norm) };
            pcm.push(s(l));
            pcm.push(s(r));
        }
        Some(pcm)
    }
}

/// Bresenham line of floor values from (x0, y0) to (x1, y1), x1 exclusive.
fn line(x0: usize, y0: i32, x1: usize, y1: i32, out: &mut [f32]) {
    let dy = y1 - y0;
    let adx = (x1 as i32 - x0 as i32).max(1);
    let ady = dy.abs();
    let base = dy / adx;
    let sy = if dy < 0 { base - 1 } else { base + 1 };
    let ady = ady - base.abs() * adx;
    let mut y = y0;
    let mut err = 0;
    let n = out.len();
    if x0 < n {
        out[x0] = INVERSE_DB[y.clamp(0, 255) as usize];
    }
    for x in x0 + 1..x1.min(n) {
        err += ady;
        if err >= adx {
            err -= adx;
            y += sy;
        } else {
            y += base;
        }
        out[x] = INVERSE_DB[y.clamp(0, 255) as usize];
    }
}

/// Split Xiph-laced packets: count-1, then sizes of all but the last.
fn xiph_unlace(d: &[u8]) -> Option<Vec<&[u8]>> {
    let n = *d.first()? as usize + 1;
    let mut p = 1;
    let mut sizes = Vec::new();
    for _ in 0..n - 1 {
        let mut s = 0usize;
        loop {
            let v = *d.get(p)? as usize;
            p += 1;
            s += v;
            if v < 255 {
                break;
            }
        }
        sizes.push(s);
    }
    let mut out = Vec::new();
    for s in sizes {
        out.push(d.get(p..p + s)?);
        p += s;
    }
    out.push(d.get(p..)?);
    Some(out)
}

impl AudioDec for VorbisDec {
    fn decode(&mut self, data: &[u8]) -> Vec<i16> {
        if !self.ready {
            // Headers may also arrive as packets.
            if data.len() > 7 && data[0] & 1 == 1 {
                self.header(data);
            }
            return Vec::new();
        }
        if data.first().is_some_and(|&b| b & 1 == 1) {
            return Vec::new();
        }
        self.packet(data).unwrap_or_default()
    }
    fn rate(&self) -> u32 {
        self.rate
    }
    fn reset(&mut self) {
        self.prev = None;
        self.first = true;
        for s in self.saved.iter_mut() {
            for v in s.iter_mut() {
                *v = 0.0;
            }
        }
    }
}
