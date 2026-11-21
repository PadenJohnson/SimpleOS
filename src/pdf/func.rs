//! PDF functions (for colour tints and gradients): sampled (type 0), exponential (2),
//! stitching (3) and PostScript calculator (4).

use alloc::boxed::Box;
use alloc::vec::Vec;

use super::file::PdfFile;
use super::obj::{Lexer, Obj, Tok};

pub enum Func {
    Sampled { size: Vec<usize>, bps: u32, domain: Vec<f32>, range: Vec<f32>, encode: Vec<f32>, decode: Vec<f32>, samples: Vec<u8>, nout: usize },
    Exp { c0: Vec<f32>, c1: Vec<f32>, n: f32, domain: [f32; 2] },
    Stitch { domain: [f32; 2], funcs: Vec<Func>, bounds: Vec<f32>, encode: Vec<f32> },
    Calc { code: Vec<Op>, nout: usize },
    /// Several functions, one output each.
    Many(Vec<Func>),
}

#[derive(Clone, Debug)]
pub enum Op {
    Num(f32),
    Name(&'static str),
    If(Box<Vec<Op>>),
    IfElse(Box<Vec<Op>>, Box<Vec<Op>>),
}

fn floats(o: &Obj) -> Vec<f32> {
    o.as_array().map(|a| a.iter().map(|x| x.as_f32().unwrap_or(0.0)).collect()).unwrap_or_default()
}

const OPS: [&str; 42] = [
    "abs", "add", "atan", "ceiling", "cos", "cvi", "cvr", "div", "exp", "floor", "idiv", "ln", "log", "mod", "mul", "neg", "round", "sin", "sqrt", "sub", "truncate", "and", "bitshift", "eq", "false", "ge", "gt", "le", "lt", "ne", "not", "or", "true", "xor", "copy", "dup", "exch", "index", "pop", "roll", "if", "ifelse",
];

fn parse_calc(lx: &mut Lexer) -> Vec<Op> {
    let mut out = Vec::new();
    let mut blocks: Vec<Vec<Op>> = Vec::new();
    while let Some(t) = lx.tok() {
        match t {
            Tok::Obj(o) => {
                if let Some(v) = o.as_f32() {
                    out.push(Op::Num(v));
                } else if let Obj::Bool(b) = o {
                    out.push(Op::Name(if b { "true" } else { "false" }));
                }
            }
            Tok::Word(b"{") => blocks.push(parse_calc(lx)),
            Tok::Word(b"}") => break,
            Tok::Word(w) => {
                let name = OPS.iter().find(|o| o.as_bytes() == w).copied();
                match name {
                    Some("if") => {
                        if let Some(b) = blocks.pop() {
                            out.push(Op::If(Box::new(b)));
                        }
                    }
                    Some("ifelse") => {
                        if blocks.len() >= 2 {
                            let e = blocks.pop().unwrap();
                            let t = blocks.pop().unwrap();
                            out.push(Op::IfElse(Box::new(t), Box::new(e)));
                        }
                    }
                    Some(n) => out.push(Op::Name(n)),
                    None => {}
                }
            }
            _ => {}
        }
    }
    out
}

fn run_calc(code: &[Op], st: &mut Vec<f32>, depth: u32) {
    if depth > 50 {
        return;
    }
    let pop = |st: &mut Vec<f32>| st.pop().unwrap_or(0.0);
    for op in code {
        if st.len() > 200 {
            return;
        }
        match op {
            Op::Num(v) => st.push(*v),
            Op::If(b) => {
                if pop(st) != 0.0 {
                    run_calc(b, st, depth + 1);
                }
            }
            Op::IfElse(t, e) => {
                if pop(st) != 0.0 {
                    run_calc(t, st, depth + 1);
                } else {
                    run_calc(e, st, depth + 1);
                }
            }
            Op::Name(n) => {
                let b2 = |st: &mut Vec<f32>, f: fn(f32, f32) -> f32| {
                    let b = pop(st);
                    let a = pop(st);
                    st.push(f(a, b));
                };
                let b1 = |st: &mut Vec<f32>, f: fn(f32) -> f32| {
                    let a = pop(st);
                    st.push(f(a));
                };
                match *n {
                    "add" => b2(st, |a, b| a + b),
                    "sub" => b2(st, |a, b| a - b),
                    "mul" => b2(st, |a, b| a * b),
                    "div" => b2(st, |a, b| if b != 0.0 { a / b } else { 0.0 }),
                    "idiv" => b2(st, |a, b| if b as i32 != 0 { (a as i32 / b as i32) as f32 } else { 0.0 }),
                    "mod" => b2(st, |a, b| if b as i32 != 0 { (a as i32 % b as i32) as f32 } else { 0.0 }),
                    "exp" => b2(st, libm::powf),
                    "atan" => b2(st, |a, b| {
                        let d = libm::atan2f(a, b).to_degrees();
                        if d < 0.0 {
                            d + 360.0
                        } else {
                            d
                        }
                    }),
                    "abs" => b1(st, |a| a.abs()),
                    "neg" => b1(st, |a| -a),
                    "ceiling" => b1(st, libm::ceilf),
                    "floor" => b1(st, libm::floorf),
                    "round" => b1(st, libm::roundf),
                    "truncate" | "cvi" => b1(st, libm::truncf),
                    "cvr" => {}
                    "sqrt" => b1(st, |a| libm::sqrtf(a.max(0.0))),
                    "sin" => b1(st, |a| libm::sinf(a.to_radians())),
                    "cos" => b1(st, |a| libm::cosf(a.to_radians())),
                    "ln" => b1(st, |a| libm::logf(a.max(1e-30))),
                    "log" => b1(st, |a| libm::log10f(a.max(1e-30))),
                    "eq" => b2(st, |a, b| (a == b) as i32 as f32),
                    "ne" => b2(st, |a, b| (a != b) as i32 as f32),
                    "gt" => b2(st, |a, b| (a > b) as i32 as f32),
                    "ge" => b2(st, |a, b| (a >= b) as i32 as f32),
                    "lt" => b2(st, |a, b| (a < b) as i32 as f32),
                    "le" => b2(st, |a, b| (a <= b) as i32 as f32),
                    "and" => b2(st, |a, b| (a as i32 & b as i32) as f32),
                    "or" => b2(st, |a, b| (a as i32 | b as i32) as f32),
                    "xor" => b2(st, |a, b| (a as i32 ^ b as i32) as f32),
                    "not" => b1(st, |a| if a == 0.0 { 1.0 } else if a == 1.0 { 0.0 } else { !(a as i32) as f32 }),
                    "bitshift" => b2(st, |a, b| if b >= 0.0 { ((a as i32) << (b as i32).min(31)) as f32 } else { ((a as i32) >> (-b as i32).min(31)) as f32 }),
                    "true" => st.push(1.0),
                    "false" => st.push(0.0),
                    "dup" => {
                        let a = *st.last().unwrap_or(&0.0);
                        st.push(a);
                    }
                    "pop" => {
                        st.pop();
                    }
                    "exch" => {
                        let n = st.len();
                        if n >= 2 {
                            st.swap(n - 1, n - 2);
                        }
                    }
                    "copy" => {
                        let k = pop(st) as usize;
                        let n = st.len();
                        if k <= n {
                            for i in n - k..n {
                                st.push(st[i]);
                            }
                        }
                    }
                    "index" => {
                        let k = pop(st) as usize;
                        let n = st.len();
                        st.push(if k < n { st[n - 1 - k] } else { 0.0 });
                    }
                    "roll" => {
                        let j = pop(st) as i32;
                        let k = pop(st) as usize;
                        let n = st.len();
                        if k > 0 && k <= n {
                            let s = &mut st[n - k..];
                            let j = j.rem_euclid(k as i32) as usize;
                            s.rotate_right(j);
                        }
                    }
                    _ => {}
                }
            }
        }
    }
}

impl Func {
    pub fn load(file: &PdfFile, o: &Obj) -> Option<Func> {
        let o = file.resolve(o);
        if let Obj::Array(a) = &o {
            let fs: Vec<Func> = a.iter().filter_map(|x| Func::load(file, x)).collect();
            return if fs.is_empty() { None } else { Some(Func::Many(fs)) };
        }
        let d = o.as_dict()?.clone();
        let domain = floats(&file.dget(&d, "Domain"));
        let range = floats(&file.dget(&d, "Range"));
        match file.dget(&d, "FunctionType").as_i64()? {
            0 => {
                let (_, samples) = file.stream_data(&o)?;
                let size: Vec<usize> = floats(&file.dget(&d, "Size")).iter().map(|&v| (v as usize).max(1)).collect();
                let bps = file.dget(&d, "BitsPerSample").as_i64().unwrap_or(8) as u32;
                let nout = range.len() / 2;
                let mut encode = floats(&file.dget(&d, "Encode"));
                if encode.is_empty() {
                    encode = size.iter().flat_map(|&s| [0.0, (s - 1) as f32]).collect();
                }
                let mut decode = floats(&file.dget(&d, "Decode"));
                if decode.is_empty() {
                    decode = range.clone();
                }
                Some(Func::Sampled { size, bps, domain, range, encode, decode, samples, nout })
            }
            2 => {
                let mut c0 = floats(&file.dget(&d, "C0"));
                let mut c1 = floats(&file.dget(&d, "C1"));
                if c0.is_empty() {
                    c0 = Vec::from([0.0]);
                }
                if c1.is_empty() {
                    c1 = Vec::from([1.0]);
                }
                let n = file.dget(&d, "N").as_f32().unwrap_or(1.0);
                Some(Func::Exp { c0, c1, n, domain: [domain.first().copied().unwrap_or(0.0), domain.get(1).copied().unwrap_or(1.0)] })
            }
            3 => {
                let funcs: Vec<Func> = file.dget(&d, "Functions").as_array().map(|a| a.iter().filter_map(|x| Func::load(file, x)).collect()).unwrap_or_default();
                Some(Func::Stitch { domain: [domain.first().copied().unwrap_or(0.0), domain.get(1).copied().unwrap_or(1.0)], funcs, bounds: floats(&file.dget(&d, "Bounds")), encode: floats(&file.dget(&d, "Encode")) })
            }
            4 => {
                let (_, code) = file.stream_data(&o)?;
                let mut lx = Lexer::new(&code, 0);
                // Skip to the opening brace.
                let mut prog = Vec::new();
                while let Some(t) = lx.tok() {
                    if t == Tok::Word(b"{") {
                        prog = parse_calc(&mut lx);
                        break;
                    }
                }
                Some(Func::Calc { code: prog, nout: (range.len() / 2).max(1) })
            }
            _ => None,
        }
    }

    pub fn eval(&self, input: &[f32]) -> Vec<f32> {
        match self {
            Func::Exp { c0, c1, n, domain } => {
                let x = input.first().copied().unwrap_or(0.0).clamp(domain[0], domain[1]);
                let t = if *n == 1.0 { x } else { libm::powf(x, *n) };
                c0.iter().zip(c1.iter()).map(|(a, b)| a + t * (b - a)).collect()
            }
            Func::Stitch { domain, funcs, bounds, encode } => {
                let x = input.first().copied().unwrap_or(0.0).clamp(domain[0], domain[1]);
                let k = bounds.iter().position(|&b| x < b).unwrap_or(bounds.len());
                let lo = if k == 0 { domain[0] } else { bounds[k - 1] };
                let hi = if k < bounds.len() { bounds[k] } else { domain[1] };
                let (e0, e1) = (encode.get(2 * k).copied().unwrap_or(0.0), encode.get(2 * k + 1).copied().unwrap_or(1.0));
                let t = if hi > lo { e0 + (x - lo) * (e1 - e0) / (hi - lo) } else { e0 };
                funcs.get(k).map(|f| f.eval(&[t])).unwrap_or_default()
            }
            Func::Sampled { size, bps, domain, range, encode, decode, samples, nout } => {
                // Nearest sample in each input dimension (linear in the first).
                let m = size.len();
                let mut idx = Vec::with_capacity(m);
                let mut frac = 0.0;
                for i in 0..m {
                    let x = input.get(i).copied().unwrap_or(0.0);
                    let (d0, d1) = (domain.get(2 * i).copied().unwrap_or(0.0), domain.get(2 * i + 1).copied().unwrap_or(1.0));
                    let (e0, e1) = (encode.get(2 * i).copied().unwrap_or(0.0), encode.get(2 * i + 1).copied().unwrap_or(1.0));
                    let x = x.clamp(d0.min(d1), d0.max(d1));
                    let e = if d1 != d0 { e0 + (x - d0) * (e1 - e0) / (d1 - d0) } else { e0 };
                    let e = e.clamp(0.0, (size[i] - 1) as f32);
                    if i == 0 {
                        frac = e - libm::floorf(e);
                    }
                    idx.push(libm::floorf(e) as usize);
                }
                let max = ((1u64 << bps) - 1) as f32;
                let sample = |idx: &[usize], j: usize| -> f32 {
                    let mut off = 0usize;
                    let mut mul = 1usize;
                    for i in 0..m {
                        off += idx[i].min(size[i] - 1) * mul;
                        mul *= size[i];
                    }
                    let bit = (off * nout + j) * *bps as usize;
                    let v = match bps {
                        8 => samples.get(bit / 8).copied().unwrap_or(0) as u32,
                        16 => samples.get(bit / 8..bit / 8 + 2).map(|s| (s[0] as u32) << 8 | s[1] as u32).unwrap_or(0),
                        _ => {
                            let byte = samples.get(bit / 8).copied().unwrap_or(0) as u32;
                            let sh = 8 - (bit % 8) as u32 - bps.min(&8);
                            (byte >> sh) & ((1 << bps.min(&8)) - 1)
                        }
                    };
                    v as f32
                };
                let mut idx2 = idx.clone();
                if m > 0 {
                    idx2[0] += 1;
                }
                (0..*nout)
                    .map(|j| {
                        let a = sample(&idx, j);
                        let b = if m > 0 && idx[0] + 1 < size[0] { sample(&idx2, j) } else { a };
                        let v = a + (b - a) * frac;
                        let (d0, d1) = (decode.get(2 * j).copied().unwrap_or(0.0), decode.get(2 * j + 1).copied().unwrap_or(1.0));
                        let out = d0 + v * (d1 - d0) / max;
                        let (r0, r1) = (range.get(2 * j).copied().unwrap_or(0.0), range.get(2 * j + 1).copied().unwrap_or(1.0));
                        out.clamp(r0.min(r1), r0.max(r1))
                    })
                    .collect()
            }
            Func::Calc { code, nout } => {
                let mut st: Vec<f32> = input.to_vec();
                run_calc(code, &mut st, 0);
                let n = st.len();
                if n >= *nout {
                    st[n - nout..].to_vec()
                } else {
                    st
                }
            }
            Func::Many(fs) => fs.iter().map(|f| f.eval(input).first().copied().unwrap_or(0.0)).collect(),
        }
    }
}
