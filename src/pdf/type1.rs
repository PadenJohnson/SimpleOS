//! Type 1 (PostScript) fonts embedded in PDFs: decrypting the program, finding its glyph
//! programs, and running them to get outlines (with flex and accented-letter "seac").

use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;
use ttf_parser::OutlineBuilder;

pub struct Type1 {
    names: Vec<String>,
    chars: Vec<Vec<u8>>,
    subrs: Vec<Vec<u8>>,
    pub encoding: Vec<Option<String>>,
}

fn decrypt(data: &[u8], key: u16, skip: usize) -> Vec<u8> {
    let mut r = key;
    let mut out = Vec::with_capacity(data.len());
    for &c in data {
        out.push(c ^ (r >> 8) as u8);
        r = (c as u16).wrapping_add(r).wrapping_mul(52845).wrapping_add(22719);
    }
    if out.len() > skip {
        out.drain(..skip);
    } else {
        out.clear();
    }
    out
}

fn find(h: &[u8], n: &[u8], from: usize) -> Option<usize> {
    if from >= h.len() {
        return None;
    }
    h[from..].windows(n.len()).position(|w| w == n).map(|i| i + from)
}

fn skip_ws(b: &[u8], mut p: usize) -> usize {
    while p < b.len() && matches!(b[p], b' ' | b'\t' | b'\r' | b'\n') {
        p += 1;
    }
    p
}

fn word(b: &[u8], p: usize) -> (&[u8], usize) {
    let s = skip_ws(b, p);
    let mut e = s;
    while e < b.len() && !matches!(b[e], b' ' | b'\t' | b'\r' | b'\n' | b'/' | b'[' | b']' | b'{' | b'}') {
        e += 1;
    }
    if e == s && e < b.len() {
        e += 1;
    }
    (&b[s..e], e)
}

fn int(w: &[u8]) -> Option<usize> {
    core::str::from_utf8(w).ok()?.parse().ok()
}

impl Type1 {
    pub fn parse(data: &[u8]) -> Option<Type1> {
        // PFB segments: strip their headers.
        let data: Vec<u8> = if data.first() == Some(&0x80) {
            let mut out = Vec::new();
            let mut p = 0;
            while p + 6 <= data.len() && data[p] == 0x80 && data[p + 1] != 3 {
                let len = u32::from_le_bytes([data[p + 2], data[p + 3], data[p + 4], data[p + 5]]) as usize;
                let end = (p + 6 + len).min(data.len());
                out.extend_from_slice(&data[p + 6..end]);
                p = end;
            }
            out
        } else {
            data.to_vec()
        };
        let ee = find(&data, b"eexec", 0)?;
        let clear = &data[..ee];
        let mut encoding = vec![None; 256];
        if find(clear, b"StandardEncoding", 0).is_some() && find(clear, b"dup", 0).is_none() {
            for (c, e) in encoding.iter_mut().enumerate() {
                *e = super::fonts::std_glyph_name(c as u8);
            }
        } else {
            let mut p = 0;
            while let Some(d) = find(clear, b"dup ", p) {
                let (w1, p1) = word(clear, d + 4);
                let (w2, p2) = word(clear, p1);
                p = p2;
                if let (Some(code), Some(name)) = (int(w1), w2.strip_prefix(b"/")) {
                    let name = if name.is_empty() { word(clear, p2).0 } else { name };
                    if code < 256 {
                        encoding[code] = Some(String::from_utf8_lossy(name).into_owned());
                    }
                }
            }
        }
        // The encrypted part: binary or hex.
        let mut s = skip_ws(&data, ee + 5);
        let mut enc = data[s..].to_vec();
        if enc.len() >= 4 && enc[..4].iter().all(|c| c.is_ascii_hexdigit()) {
            let mut out = Vec::new();
            let mut hi: Option<u8> = None;
            for &c in &enc {
                let d = match c {
                    b'0'..=b'9' => c - b'0',
                    b'a'..=b'f' => c - b'a' + 10,
                    b'A'..=b'F' => c - b'A' + 10,
                    _ => continue,
                };
                match hi.take() {
                    Some(h) => out.push(h << 4 | d),
                    None => hi = Some(d),
                }
            }
            enc = out;
            s = 0;
        }
        let _ = s;
        let priv_ = decrypt(&enc, 55665, 4);
        let len_iv = find(&priv_, b"/lenIV", 0).and_then(|p| int(word(&priv_, p + 6).0)).unwrap_or(4);
        // Subrs.
        let mut subrs = Vec::new();
        if let Some(p) = find(&priv_, b"/Subrs", 0) {
            let (n, mut p) = word(&priv_, p + 6);
            let n = int(n).unwrap_or(0).min(10000);
            subrs = vec![Vec::new(); n];
            for _ in 0..n {
                let (w, p1) = word(&priv_, p);
                if w != b"dup" {
                    break;
                }
                let (idx, p2) = word(&priv_, p1);
                let (len, p3) = word(&priv_, p2);
                let (_rd, p4) = word(&priv_, p3);
                let (Some(idx), Some(len)) = (int(idx), int(len)) else { break };
                let start = p4 + 1;
                if start + len > priv_.len() {
                    break;
                }
                if idx < subrs.len() {
                    subrs[idx] = decrypt(&priv_[start..start + len], 4330, len_iv);
                }
                p = start + len;
                // Skip "NP" / "noaccess put".
                let (w, p5) = word(&priv_, p);
                p = if w == b"noaccess" { word(&priv_, p5).1 } else { p5 };
            }
        }
        // CharStrings.
        let mut names = Vec::new();
        let mut chars = Vec::new();
        let cs = find(&priv_, b"/CharStrings", 0)?;
        let mut p = cs + 12;
        // Skip to "begin".
        p = find(&priv_, b"begin", p).map(|x| x + 5).unwrap_or(p);
        loop {
            let (w, p1) = word(&priv_, p);
            if w != b"/" {
                break;
            }
            let (name, p2) = word(&priv_, p1);
            let (len, p3) = word(&priv_, p2);
            let (_rd, p4) = word(&priv_, p3);
            let Some(len) = int(len) else { break };
            let start = p4 + 1;
            if start + len > priv_.len() {
                break;
            }
            names.push(String::from_utf8_lossy(name).into_owned());
            chars.push(decrypt(&priv_[start..start + len], 4330, len_iv));
            p = start + len;
            let (_nd, p5) = word(&priv_, p);
            p = p5;
            if names.len() > 5000 {
                break;
            }
        }
        if names.is_empty() {
            return None;
        }
        Some(Type1 { names, chars, subrs, encoding })
    }

    pub fn glyph_index(&self, name: &str) -> Option<usize> {
        self.names.iter().position(|n| n == name)
    }

    pub fn width(&self, gid: usize) -> f32 {
        let mut sink = Sink;
        self.run(gid, &mut sink, 0.0, 0.0, 0).unwrap_or(500.0)
    }

    pub fn outline(&self, gid: usize, b: &mut dyn OutlineBuilder) {
        let _ = self.run(gid, b, 0.0, 0.0, 0);
    }

    /// Run a glyph program; returns its advance width.
    fn run(&self, gid: usize, b: &mut dyn OutlineBuilder, dx: f32, dy: f32, depth: u32) -> Option<f32> {
        let prog = self.chars.get(gid)?;
        let mut st = State { stack: Vec::new(), ps: Vec::new(), x: 0.0, y: 0.0, w: 0.0, open: false, flex: None, dx, dy, depth };
        self.exec(prog, &mut st, b, 0)?;
        if st.open {
            b.close();
        }
        Some(st.w)
    }

    fn exec(&self, prog: &[u8], s: &mut State, b: &mut dyn OutlineBuilder, depth: u32) -> Option<bool> {
        if depth > 10 {
            return None;
        }
        let mut i = 0;
        while i < prog.len() {
            let v = prog[i];
            i += 1;
            match v {
                32..=246 => s.stack.push(v as f32 - 139.0),
                247..=250 => {
                    let w = *prog.get(i)? as f32;
                    i += 1;
                    s.stack.push((v as f32 - 247.0) * 256.0 + w + 108.0);
                }
                251..=254 => {
                    let w = *prog.get(i)? as f32;
                    i += 1;
                    s.stack.push(-(v as f32 - 251.0) * 256.0 - w - 108.0);
                }
                255 => {
                    let n = i32::from_be_bytes(prog.get(i..i + 4)?.try_into().ok()?);
                    i += 4;
                    s.stack.push(n as f32);
                }
                12 => {
                    let e = *prog.get(i)?;
                    i += 1;
                    match e {
                        6 => {
                            // seac: asb adx ady bchar achar
                            if s.stack.len() >= 5 {
                                let n = s.stack.len();
                                let (asb, adx, ady, bc, ac) = (s.stack[n - 5], s.stack[n - 4], s.stack[n - 3], s.stack[n - 2] as u8, s.stack[n - 1] as u8);
                                let base = super::fonts::std_glyph_name(bc).and_then(|n| self.glyph_index(&n));
                                let acc = super::fonts::std_glyph_name(ac).and_then(|n| self.glyph_index(&n));
                                if let Some(g) = base {
                                    self.run(g, b, s.dx, s.dy, s.depth + 1);
                                }
                                if let Some(g) = acc {
                                    self.run(g, b, s.dx + adx - asb, s.dy + ady, s.depth + 1);
                                }
                            }
                            return Some(true);
                        }
                        7 => {
                            // sbw
                            if s.stack.len() >= 4 {
                                let n = s.stack.len();
                                s.x = s.stack[n - 4];
                                s.y = s.stack[n - 3];
                                s.w = s.stack[n - 2];
                            }
                            s.stack.clear();
                        }
                        12 => {
                            if s.stack.len() >= 2 {
                                let d = s.stack.pop().unwrap();
                                let n = s.stack.pop().unwrap();
                                s.stack.push(if d != 0.0 { n / d } else { 0.0 });
                            }
                        }
                        16 => {
                            // callothersubr: othersubr# argc args...
                            let num = s.stack.pop().unwrap_or(0.0) as i32;
                            let argc = (s.stack.pop().unwrap_or(0.0) as usize).min(s.stack.len());
                            let args: Vec<f32> = s.stack.split_off(s.stack.len() - argc);
                            match num {
                                1 => s.flex = Some(Vec::new()),
                                2 => {}
                                0 => {
                                    if let Some(pts) = s.flex.take() {
                                        if pts.len() >= 7 {
                                            let p = |k: usize| (pts[k].0 + s.dx, pts[k].1 + s.dy);
                                            let (a, bb, c) = (p(1), p(2), p(3));
                                            let (d, e, f) = (p(4), p(5), p(6));
                                            b.curve_to(a.0, a.1, bb.0, bb.1, c.0, c.1);
                                            b.curve_to(d.0, d.1, e.0, e.1, f.0, f.1);
                                            s.x = pts[6].0;
                                            s.y = pts[6].1;
                                        }
                                    }
                                    if args.len() >= 3 {
                                        s.ps.push(args[2]);
                                        s.ps.push(args[1]);
                                    }
                                }
                                _ => {
                                    for a in args.iter().rev() {
                                        s.ps.push(*a);
                                    }
                                }
                            }
                        }
                        17 => {
                            let v = s.ps.pop().unwrap_or(0.0);
                            s.stack.push(v);
                        }
                        33 => {
                            if s.stack.len() >= 2 {
                                let n = s.stack.len();
                                s.x = s.stack[n - 2];
                                s.y = s.stack[n - 1];
                            }
                            s.stack.clear();
                        }
                        _ => s.stack.clear(),
                    }
                }
                13 => {
                    // hsbw
                    if s.stack.len() >= 2 {
                        let n = s.stack.len();
                        s.x = s.stack[n - 2];
                        s.y = 0.0;
                        s.w = s.stack[n - 1];
                    }
                    s.stack.clear();
                }
                21 | 22 | 4 => {
                    let (ddx, ddy) = match v {
                        21 => (s.arg(0, 2), s.arg(1, 2)),
                        22 => (s.arg(0, 1), 0.0),
                        _ => (0.0, s.arg(0, 1)),
                    };
                    s.x += ddx;
                    s.y += ddy;
                    s.stack.clear();
                    if let Some(f) = &mut s.flex {
                        f.push((s.x, s.y));
                    } else {
                        if s.open {
                            b.close();
                        }
                        b.move_to(s.x + s.dx, s.y + s.dy);
                        s.open = true;
                    }
                }
                5 | 6 | 7 => {
                    let (ddx, ddy) = match v {
                        5 => (s.arg(0, 2), s.arg(1, 2)),
                        6 => (s.arg(0, 1), 0.0),
                        _ => (0.0, s.arg(0, 1)),
                    };
                    s.x += ddx;
                    s.y += ddy;
                    s.stack.clear();
                    b.line_to(s.x + s.dx, s.y + s.dy);
                }
                8 | 30 | 31 => {
                    let (d1x, d1y, d2x, d2y, d3x, d3y) = match v {
                        8 => (s.arg(0, 6), s.arg(1, 6), s.arg(2, 6), s.arg(3, 6), s.arg(4, 6), s.arg(5, 6)),
                        30 => (0.0, s.arg(0, 4), s.arg(1, 4), s.arg(2, 4), s.arg(3, 4), 0.0),
                        _ => (s.arg(0, 4), 0.0, s.arg(1, 4), s.arg(2, 4), 0.0, s.arg(3, 4)),
                    };
                    let (x1, y1) = (s.x + d1x, s.y + d1y);
                    let (x2, y2) = (x1 + d2x, y1 + d2y);
                    let (x3, y3) = (x2 + d3x, y2 + d3y);
                    b.curve_to(x1 + s.dx, y1 + s.dy, x2 + s.dx, y2 + s.dy, x3 + s.dx, y3 + s.dy);
                    s.x = x3;
                    s.y = y3;
                    s.stack.clear();
                }
                9 => {
                    if s.open {
                        b.close();
                        s.open = false;
                    }
                    s.stack.clear();
                }
                10 => {
                    let n = s.stack.pop().unwrap_or(-1.0) as i32;
                    if n >= 0 {
                        if let Some(sub) = self.subrs.get(n as usize) {
                            if let Some(true) = self.exec(sub, s, b, depth + 1) {
                                return Some(true);
                            }
                        }
                    }
                }
                11 => return Some(false),
                14 => return Some(true),
                1 | 3 => s.stack.clear(),
                _ => s.stack.clear(),
            }
        }
        Some(false)
    }
}

struct State {
    stack: Vec<f32>,
    ps: Vec<f32>,
    x: f32,
    y: f32,
    w: f32,
    open: bool,
    flex: Option<Vec<(f32, f32)>>,
    dx: f32,
    dy: f32,
    depth: u32,
}

impl State {
    /// Argument k of an n-argument command (taken from the top of the stack).
    fn arg(&self, k: usize, n: usize) -> f32 {
        let len = self.stack.len();
        if len < n {
            return 0.0;
        }
        self.stack[len - n + k]
    }
}

struct Sink;
impl OutlineBuilder for Sink {
    fn move_to(&mut self, _: f32, _: f32) {}
    fn line_to(&mut self, _: f32, _: f32) {}
    fn quad_to(&mut self, _: f32, _: f32, _: f32, _: f32) {}
    fn curve_to(&mut self, _: f32, _: f32, _: f32, _: f32, _: f32, _: f32) {}
    fn close(&mut self) {}
}
