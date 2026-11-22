//! VP9 pixel kernels for 8-bit pictures: intra prediction, inverse transforms and the
//! loop filter (ported from FFmpeg's vp9dsp_template.c, LGPL 2.1 or later).

// Intra modes.
pub const VERT: usize = 0;
pub const HOR: usize = 1;
pub const DC: usize = 2;
pub const DIAG_DOWN_LEFT: usize = 3;
pub const DIAG_DOWN_RIGHT: usize = 4;
pub const VERT_RIGHT: usize = 5;
pub const HOR_DOWN: usize = 6;
pub const VERT_LEFT: usize = 7;
pub const HOR_UP: usize = 8;
pub const TM: usize = 9;
pub const LEFT_DC: usize = 10;
pub const TOP_DC: usize = 11;
pub const DC_128: usize = 12;
pub const DC_127: usize = 13;
pub const DC_129: usize = 14;

#[inline]
fn clip(v: i32) -> u8 {
    v.clamp(0, 255) as u8
}

/// Intra prediction of a size x size block at `o`. `l`: left column (bottom to top, so
/// l[size-1] is the top one; top to bottom for HOR_UP). `a`: a[0] top-left, a[1..] the row
/// above (and top-right for 4x4).
pub fn intra_pred(mode: usize, size: usize, d: &mut [u8], o: usize, stride: usize, l: &[u8], a: &[u8]) {
    let top = |i: isize| a[(i + 1) as usize] as i32;
    let left = |i: usize| l[i] as i32;
    let tl = a[0] as i32;
    let fill = |d: &mut [u8], v: u8| {
        for y in 0..size {
            d[o + y * stride..o + y * stride + size].iter_mut().for_each(|p| *p = v);
        }
    };
    let log2 = size.trailing_zeros();
    macro_rules! dst {
        ($x:expr, $y:expr) => {
            d[o + ($x) + ($y) * stride]
        };
    }
    match mode {
        VERT => {
            for y in 0..size {
                d[o + y * stride..o + y * stride + size].copy_from_slice(&a[1..1 + size]);
            }
        }
        HOR => {
            for y in 0..size {
                let v = l[size - 1 - y];
                d[o + y * stride..o + y * stride + size].iter_mut().for_each(|p| *p = v);
            }
        }
        TM => {
            for y in 0..size {
                let lm = left(size - 1 - y) - tl;
                for x in 0..size {
                    dst!(x, y) = clip(top(x as isize) + lm);
                }
            }
        }
        DC => {
            let s: i32 = l[..size].iter().map(|&v| v as i32).sum::<i32>() + a[1..1 + size].iter().map(|&v| v as i32).sum::<i32>();
            fill(d, ((s + size as i32) >> (log2 + 1)) as u8);
        }
        LEFT_DC => {
            let s: i32 = l[..size].iter().map(|&v| v as i32).sum();
            fill(d, ((s + (size as i32 >> 1)) >> log2) as u8);
        }
        TOP_DC => {
            let s: i32 = a[1..1 + size].iter().map(|&v| v as i32).sum();
            fill(d, ((s + (size as i32 >> 1)) >> log2) as u8);
        }
        DC_128 => fill(d, 128),
        DC_127 => fill(d, 127),
        DC_129 => fill(d, 129),
        DIAG_DOWN_LEFT => {
            if size == 4 {
                let t: [i32; 8] = core::array::from_fn(|i| top(i as isize));
                let f = |a: i32, b: i32, c: i32| ((a + b * 2 + c + 2) >> 2) as u8;
                dst!(0, 0) = f(t[0], t[1], t[2]);
                let v1 = f(t[1], t[2], t[3]);
                dst!(1, 0) = v1;
                dst!(0, 1) = v1;
                let v2 = f(t[2], t[3], t[4]);
                dst!(2, 0) = v2;
                dst!(1, 1) = v2;
                dst!(0, 2) = v2;
                let v3 = f(t[3], t[4], t[5]);
                dst!(3, 0) = v3;
                dst!(2, 1) = v3;
                dst!(1, 2) = v3;
                dst!(0, 3) = v3;
                let v4 = f(t[4], t[5], t[6]);
                dst!(3, 1) = v4;
                dst!(2, 2) = v4;
                dst!(1, 3) = v4;
                let v5 = f(t[5], t[6], t[7]);
                dst!(3, 2) = v5;
                dst!(2, 3) = v5;
                dst!(3, 3) = t[7] as u8;
            } else {
                let mut v = [0u8; 32];
                for i in 0..size - 2 {
                    v[i] = ((top(i as isize) + top(i as isize + 1) * 2 + top(i as isize + 2) + 2) >> 2) as u8;
                }
                v[size - 2] = ((top(size as isize - 2) + top(size as isize - 1) * 3 + 2) >> 2) as u8;
                let last = a[size];
                for j in 0..size {
                    let r = o + j * stride;
                    d[r..r + size - 1 - j].copy_from_slice(&v[j..size - 1]);
                    d[r + size - 1 - j..r + size].iter_mut().for_each(|p| *p = last);
                }
            }
        }
        DIAG_DOWN_RIGHT => {
            if size == 4 {
                let (a0, a1, a2, a3) = (top(0), top(1), top(2), top(3));
                let (l0, l1, l2, l3) = (left(3), left(2), left(1), left(0));
                let f = |a: i32, b: i32, c: i32| ((a + b * 2 + c + 2) >> 2) as u8;
                dst!(0, 3) = f(l1, l2, l3);
                let v = f(l0, l1, l2);
                dst!(0, 2) = v;
                dst!(1, 3) = v;
                let v = f(tl, l0, l1);
                dst!(0, 1) = v;
                dst!(1, 2) = v;
                dst!(2, 3) = v;
                let v = f(l0, tl, a0);
                dst!(0, 0) = v;
                dst!(1, 1) = v;
                dst!(2, 2) = v;
                dst!(3, 3) = v;
                let v = f(tl, a0, a1);
                dst!(1, 0) = v;
                dst!(2, 1) = v;
                dst!(3, 2) = v;
                let v = f(a0, a1, a2);
                dst!(2, 0) = v;
                dst!(3, 1) = v;
                dst!(3, 0) = f(a1, a2, a3);
            } else {
                let mut v = [0u8; 64];
                for i in 0..size - 2 {
                    v[i] = ((left(i) + left(i + 1) * 2 + left(i + 2) + 2) >> 2) as u8;
                    v[size + 1 + i] = ((top(i as isize) + top(i as isize + 1) * 2 + top(i as isize + 2) + 2) >> 2) as u8;
                }
                v[size - 2] = ((left(size - 2) + left(size - 1) * 2 + tl + 2) >> 2) as u8;
                v[size - 1] = ((left(size - 1) + tl * 2 + top(0) + 2) >> 2) as u8;
                v[size] = ((tl + top(0) * 2 + top(1) + 2) >> 2) as u8;
                for j in 0..size {
                    let r = o + j * stride;
                    d[r..r + size].copy_from_slice(&v[size - 1 - j..2 * size - 1 - j]);
                }
            }
        }
        VERT_RIGHT => {
            if size == 4 {
                let (a0, a1, a2, a3) = (top(0), top(1), top(2), top(3));
                let (l0, l1, l2) = (left(3), left(2), left(1));
                let f = |a: i32, b: i32, c: i32| ((a + b * 2 + c + 2) >> 2) as u8;
                let g = |a: i32, b: i32| ((a + b + 1) >> 1) as u8;
                dst!(0, 3) = f(l0, l1, l2);
                dst!(0, 2) = f(tl, l0, l1);
                let v = g(tl, a0);
                dst!(0, 0) = v;
                dst!(1, 2) = v;
                let v = f(l0, tl, a0);
                dst!(0, 1) = v;
                dst!(1, 3) = v;
                let v = g(a0, a1);
                dst!(1, 0) = v;
                dst!(2, 2) = v;
                let v = f(tl, a0, a1);
                dst!(1, 1) = v;
                dst!(2, 3) = v;
                let v = g(a1, a2);
                dst!(2, 0) = v;
                dst!(3, 2) = v;
                let v = f(a0, a1, a2);
                dst!(2, 1) = v;
                dst!(3, 3) = v;
                dst!(3, 0) = g(a2, a3);
                dst!(3, 1) = f(a1, a2, a3);
            } else {
                let h = size / 2;
                let mut ve = [0u8; 48];
                let mut vo = [0u8; 48];
                for i in 0..h - 2 {
                    vo[i] = ((left(i * 2 + 3) + left(i * 2 + 2) * 2 + left(i * 2 + 1) + 2) >> 2) as u8;
                    ve[i] = ((left(i * 2 + 4) + left(i * 2 + 3) * 2 + left(i * 2 + 2) + 2) >> 2) as u8;
                }
                vo[h - 2] = ((left(size - 1) + left(size - 2) * 2 + left(size - 3) + 2) >> 2) as u8;
                ve[h - 2] = ((tl + left(size - 1) * 2 + left(size - 2) + 2) >> 2) as u8;
                ve[h - 1] = ((tl + top(0) + 1) >> 1) as u8;
                vo[h - 1] = ((left(size - 1) + tl * 2 + top(0) + 2) >> 2) as u8;
                for i in 0..size - 1 {
                    ve[h + i] = ((top(i as isize) + top(i as isize + 1) + 1) >> 1) as u8;
                    vo[h + i] = ((top(i as isize - 1) + top(i as isize) * 2 + top(i as isize + 1) + 2) >> 2) as u8;
                }
                for j in 0..h {
                    let r = o + j * 2 * stride;
                    d[r..r + size].copy_from_slice(&ve[h - 1 - j..h - 1 - j + size]);
                    let r = r + stride;
                    d[r..r + size].copy_from_slice(&vo[h - 1 - j..h - 1 - j + size]);
                }
            }
        }
        HOR_DOWN => {
            if size == 4 {
                let (l0, l1, l2, l3) = (left(3), left(2), left(1), left(0));
                let (a0, a1, a2) = (top(0), top(1), top(2));
                let f = |a: i32, b: i32, c: i32| ((a + b * 2 + c + 2) >> 2) as u8;
                let g = |a: i32, b: i32| ((a + b + 1) >> 1) as u8;
                dst!(2, 0) = f(tl, a0, a1);
                dst!(3, 0) = f(a0, a1, a2);
                let v = g(tl, l0);
                dst!(0, 0) = v;
                dst!(2, 1) = v;
                let v = f(a0, tl, l0);
                dst!(1, 0) = v;
                dst!(3, 1) = v;
                let v = g(l0, l1);
                dst!(0, 1) = v;
                dst!(2, 2) = v;
                let v = f(tl, l0, l1);
                dst!(1, 1) = v;
                dst!(3, 2) = v;
                let v = g(l1, l2);
                dst!(0, 2) = v;
                dst!(2, 3) = v;
                let v = f(l0, l1, l2);
                dst!(1, 2) = v;
                dst!(3, 3) = v;
                dst!(0, 3) = g(l2, l3);
                dst!(1, 3) = f(l1, l2, l3);
            } else {
                let mut v = [0u8; 96];
                for i in 0..size - 2 {
                    v[i * 2] = ((left(i + 1) + left(i) + 1) >> 1) as u8;
                    v[i * 2 + 1] = ((left(i + 2) + left(i + 1) * 2 + left(i) + 2) >> 2) as u8;
                    v[size * 2 + i] = ((top(i as isize - 1) + top(i as isize) * 2 + top(i as isize + 1) + 2) >> 2) as u8;
                }
                v[size * 2 - 2] = ((tl + left(size - 1) + 1) >> 1) as u8;
                v[size * 2 - 4] = ((left(size - 1) + left(size - 2) + 1) >> 1) as u8;
                v[size * 2 - 1] = ((top(0) + tl * 2 + left(size - 1) + 2) >> 2) as u8;
                v[size * 2 - 3] = ((tl + left(size - 1) * 2 + left(size - 2) + 2) >> 2) as u8;
                for j in 0..size {
                    let r = o + j * stride;
                    let s = size * 2 - 2 - j * 2;
                    d[r..r + size].copy_from_slice(&v[s..s + size]);
                }
            }
        }
        VERT_LEFT => {
            if size == 4 {
                let t: [i32; 7] = core::array::from_fn(|i| top(i as isize));
                let f = |a: i32, b: i32, c: i32| ((a + b * 2 + c + 2) >> 2) as u8;
                let g = |a: i32, b: i32| ((a + b + 1) >> 1) as u8;
                dst!(0, 0) = g(t[0], t[1]);
                dst!(0, 1) = f(t[0], t[1], t[2]);
                let v = g(t[1], t[2]);
                dst!(1, 0) = v;
                dst!(0, 2) = v;
                let v = f(t[1], t[2], t[3]);
                dst!(1, 1) = v;
                dst!(0, 3) = v;
                let v = g(t[2], t[3]);
                dst!(2, 0) = v;
                dst!(1, 2) = v;
                let v = f(t[2], t[3], t[4]);
                dst!(2, 1) = v;
                dst!(1, 3) = v;
                let v = g(t[3], t[4]);
                dst!(3, 0) = v;
                dst!(2, 2) = v;
                let v = f(t[3], t[4], t[5]);
                dst!(3, 1) = v;
                dst!(2, 3) = v;
                dst!(3, 2) = g(t[4], t[5]);
                dst!(3, 3) = f(t[4], t[5], t[6]);
            } else {
                let mut ve = [0u8; 32];
                let mut vo = [0u8; 32];
                for i in 0..size - 2 {
                    ve[i] = ((top(i as isize) + top(i as isize + 1) + 1) >> 1) as u8;
                    vo[i] = ((top(i as isize) + top(i as isize + 1) * 2 + top(i as isize + 2) + 2) >> 2) as u8;
                }
                ve[size - 2] = ((top(size as isize - 2) + top(size as isize - 1) + 1) >> 1) as u8;
                vo[size - 2] = ((top(size as isize - 2) + top(size as isize - 1) * 3 + 2) >> 2) as u8;
                let last = a[size];
                for j in 0..size / 2 {
                    let r = o + j * 2 * stride;
                    d[r..r + size - j - 1].copy_from_slice(&ve[j..size - 1]);
                    d[r + size - j - 1..r + size].iter_mut().for_each(|p| *p = last);
                    let r = r + stride;
                    d[r..r + size - j - 1].copy_from_slice(&vo[j..size - 1]);
                    d[r + size - j - 1..r + size].iter_mut().for_each(|p| *p = last);
                }
            }
        }
        HOR_UP => {
            if size == 4 {
                let (l0, l1, l2, l3) = (left(0), left(1), left(2), left(3));
                let f = |a: i32, b: i32, c: i32| ((a + b * 2 + c + 2) >> 2) as u8;
                let g = |a: i32, b: i32| ((a + b + 1) >> 1) as u8;
                dst!(0, 0) = g(l0, l1);
                dst!(1, 0) = f(l0, l1, l2);
                let v = g(l1, l2);
                dst!(0, 1) = v;
                dst!(2, 0) = v;
                let v = f(l1, l2, l3);
                dst!(1, 1) = v;
                dst!(3, 0) = v;
                let v = g(l2, l3);
                dst!(0, 2) = v;
                dst!(2, 1) = v;
                let v = ((l2 + l3 * 3 + 2) >> 2) as u8;
                dst!(1, 2) = v;
                dst!(3, 1) = v;
                let v = l3 as u8;
                dst!(0, 3) = v;
                dst!(1, 3) = v;
                dst!(2, 2) = v;
                dst!(2, 3) = v;
                dst!(3, 2) = v;
                dst!(3, 3) = v;
            } else {
                let mut v = [0u8; 64];
                for i in 0..size - 2 {
                    v[i * 2] = ((left(i) + left(i + 1) + 1) >> 1) as u8;
                    v[i * 2 + 1] = ((left(i) + left(i + 1) * 2 + left(i + 2) + 2) >> 2) as u8;
                }
                v[size * 2 - 4] = ((left(size - 2) + left(size - 1) + 1) >> 1) as u8;
                v[size * 2 - 3] = ((left(size - 2) + left(size - 1) * 3 + 2) >> 2) as u8;
                for j in 0..size / 2 {
                    let r = o + j * stride;
                    d[r..r + size].copy_from_slice(&v[j * 2..j * 2 + size]);
                }
                let last = l[size - 1];
                for j in size / 2..size {
                    let r = o + j * stride;
                    let n = size * 2 - 2 - j * 2;
                    d[r..r + n].copy_from_slice(&v[j * 2..j * 2 + n]);
                    d[r + n..r + size].iter_mut().for_each(|p| *p = last);
                }
            }
        }
        _ => {}
    }
}

// ---- Inverse transforms ----

#[inline]
fn r14(v: i32) -> i32 {
    v.wrapping_add(1 << 13) >> 14
}
#[inline]
fn m(a: i32, b: i32) -> i32 {
    a.wrapping_mul(b)
}

fn idct4(i: &[i32], o: &mut [i32]) {
    let t0 = r14(m(i[0] + i[2], 11585));
    let t1 = r14(m(i[0] - i[2], 11585));
    let t2 = r14(m(i[1], 6270) - m(i[3], 15137));
    let t3 = r14(m(i[1], 15137) + m(i[3], 6270));
    o[0] = t0 + t3;
    o[1] = t1 + t2;
    o[2] = t1 - t2;
    o[3] = t0 - t3;
}

fn iadst4(i: &[i32], o: &mut [i32]) {
    let t0 = 5283 * i[0] + 15212 * i[2] + 9929 * i[3];
    let t1 = 9929 * i[0] - 5283 * i[2] - 15212 * i[3];
    let t2 = 13377 * (i[0] - i[2] + i[3]);
    let t3 = 13377 * i[1];
    o[0] = r14(t0 + t3);
    o[1] = r14(t1 + t3);
    o[2] = r14(t2);
    o[3] = r14(t0 + t1 - t3);
}

fn idct8(i: &[i32], o: &mut [i32]) {
    let t0a = r14(m(i[0] + i[4], 11585));
    let t1a = r14(m(i[0] - i[4], 11585));
    let t2a = r14(m(i[2], 6270) - m(i[6], 15137));
    let t3a = r14(m(i[2], 15137) + m(i[6], 6270));
    let t4a = r14(m(i[1], 3196) - m(i[7], 16069));
    let t5a = r14(m(i[5], 13623) - m(i[3], 9102));
    let t6a = r14(m(i[5], 9102) + m(i[3], 13623));
    let t7a = r14(m(i[1], 16069) + m(i[7], 3196));
    let t0 = t0a + t3a;
    let t1 = t1a + t2a;
    let t2 = t1a - t2a;
    let t3 = t0a - t3a;
    let t4 = t4a + t5a;
    let t5a = t4a - t5a;
    let t7 = t7a + t6a;
    let t6a = t7a - t6a;
    let t5 = r14(m(t6a - t5a, 11585));
    let t6 = r14(m(t6a + t5a, 11585));
    o[0] = t0 + t7;
    o[1] = t1 + t6;
    o[2] = t2 + t5;
    o[3] = t3 + t4;
    o[4] = t3 - t4;
    o[5] = t2 - t5;
    o[6] = t1 - t6;
    o[7] = t0 - t7;
}

fn iadst8(i: &[i32], o: &mut [i32]) {
    let t0a = m(16305, i[7]) + m(1606, i[0]);
    let t1a = m(1606, i[7]) - m(16305, i[0]);
    let t2a = m(14449, i[5]) + m(7723, i[2]);
    let t3a = m(7723, i[5]) - m(14449, i[2]);
    let t4a = m(10394, i[3]) + m(12665, i[4]);
    let t5a = m(12665, i[3]) - m(10394, i[4]);
    let t6a = m(4756, i[1]) + m(15679, i[6]);
    let t7a = m(15679, i[1]) - m(4756, i[6]);
    let t0 = r14(t0a + t4a);
    let t1 = r14(t1a + t5a);
    let t2 = r14(t2a + t6a);
    let t3 = r14(t3a + t7a);
    let t4 = r14(t0a - t4a);
    let t5 = r14(t1a - t5a);
    let t6 = r14(t2a - t6a);
    let t7 = r14(t3a - t7a);
    let t4a = m(15137, t4).wrapping_add(m(6270, t5));
    let t5a = m(6270, t4).wrapping_sub(m(15137, t5));
    let t6a = m(15137, t7).wrapping_sub(m(6270, t6));
    let t7a = m(6270, t7).wrapping_add(m(15137, t6));
    o[0] = t0 + t2;
    o[7] = -(t1 + t3);
    let t2 = t0 - t2;
    let t3 = t1 - t3;
    o[1] = -r14(t4a.wrapping_add(t6a));
    o[6] = r14(t5a.wrapping_add(t7a));
    let t6 = r14(t4a.wrapping_sub(t6a));
    let t7 = r14(t5a.wrapping_sub(t7a));
    o[3] = -r14(m(t2 + t3, 11585));
    o[4] = r14(m(t2 - t3, 11585));
    o[2] = r14(m(t6 + t7, 11585));
    o[5] = -r14(m(t6 - t7, 11585));
}

fn idct16(i: &[i32], o: &mut [i32]) {
    let t0a = r14(m(i[0] + i[8], 11585));
    let t1a = r14(m(i[0] - i[8], 11585));
    let t2a = r14(m(i[4], 6270).wrapping_sub(m(i[12], 15137)));
    let t3a = r14(m(i[4], 15137).wrapping_add(m(i[12], 6270)));
    let t4a = r14(m(i[2], 3196).wrapping_sub(m(i[14], 16069)));
    let t7a = r14(m(i[2], 16069).wrapping_add(m(i[14], 3196)));
    let t5a = r14(m(i[10], 13623).wrapping_sub(m(i[6], 9102)));
    let t6a = r14(m(i[10], 9102).wrapping_add(m(i[6], 13623)));
    let t8a = r14(m(i[1], 1606).wrapping_sub(m(i[15], 16305)));
    let t15a = r14(m(i[1], 16305).wrapping_add(m(i[15], 1606)));
    let t9a = r14(m(i[9], 12665).wrapping_sub(m(i[7], 10394)));
    let t14a = r14(m(i[9], 10394).wrapping_add(m(i[7], 12665)));
    let t10a = r14(m(i[5], 7723).wrapping_sub(m(i[11], 14449)));
    let t13a = r14(m(i[5], 14449).wrapping_add(m(i[11], 7723)));
    let t11a = r14(m(i[13], 15679).wrapping_sub(m(i[3], 4756)));
    let t12a = r14(m(i[13], 4756).wrapping_add(m(i[3], 15679)));
    let t0 = t0a + t3a;
    let t1 = t1a + t2a;
    let t2 = t1a - t2a;
    let t3 = t0a - t3a;
    let t4 = t4a + t5a;
    let t5 = t4a - t5a;
    let t6 = t7a - t6a;
    let t7 = t7a + t6a;
    let t8 = t8a + t9a;
    let t9 = t8a - t9a;
    let t10 = t11a - t10a;
    let t11 = t11a + t10a;
    let t12 = t12a + t13a;
    let t13 = t12a - t13a;
    let t14 = t15a - t14a;
    let t15 = t15a + t14a;
    let t5a = r14(m(t6 - t5, 11585));
    let t6a = r14(m(t6 + t5, 11585));
    let t9a = r14(m(t14, 6270).wrapping_sub(m(t9, 15137)));
    let t14a = r14(m(t14, 15137).wrapping_add(m(t9, 6270)));
    let t10a = r14((m(t13, 15137).wrapping_add(m(t10, 6270))).wrapping_neg());
    let t13a = r14(m(t13, 6270).wrapping_sub(m(t10, 15137)));
    let t0a = t0 + t7;
    let t1a = t1 + t6a;
    let t2a = t2 + t5a;
    let t3a = t3 + t4;
    let t4 = t3 - t4;
    let t5 = t2 - t5a;
    let t6 = t1 - t6a;
    let t7 = t0 - t7;
    let t8a = t8 + t11;
    let t9 = t9a + t10a;
    let t10 = t9a - t10a;
    let t11a = t8 - t11;
    let t12a = t15 - t12;
    let t13 = t14a - t13a;
    let t14 = t14a + t13a;
    let t15a = t15 + t12;
    let t10a = r14(m(t13 - t10, 11585));
    let t13a = r14(m(t13 + t10, 11585));
    let t11 = r14(m(t12a - t11a, 11585));
    let t12 = r14(m(t12a + t11a, 11585));
    o[0] = t0a + t15a;
    o[1] = t1a + t14;
    o[2] = t2a + t13a;
    o[3] = t3a + t12;
    o[4] = t4 + t11;
    o[5] = t5 + t10a;
    o[6] = t6 + t9;
    o[7] = t7 + t8a;
    o[8] = t7 - t8a;
    o[9] = t6 - t9;
    o[10] = t5 - t10a;
    o[11] = t4 - t11;
    o[12] = t3a - t12;
    o[13] = t2a - t13a;
    o[14] = t1a - t14;
    o[15] = t0a - t15a;
}

fn iadst16(i: &[i32], o: &mut [i32]) {
    let t0 = m(i[15], 16364).wrapping_add(m(i[0], 804));
    let t1 = m(i[15], 804).wrapping_sub(m(i[0], 16364));
    let t2 = m(i[13], 15893).wrapping_add(m(i[2], 3981));
    let t3 = m(i[13], 3981).wrapping_sub(m(i[2], 15893));
    let t4 = m(i[11], 14811).wrapping_add(m(i[4], 7005));
    let t5 = m(i[11], 7005).wrapping_sub(m(i[4], 14811));
    let t6 = m(i[9], 13160).wrapping_add(m(i[6], 9760));
    let t7 = m(i[9], 9760).wrapping_sub(m(i[6], 13160));
    let t8 = m(i[7], 11003).wrapping_add(m(i[8], 12140));
    let t9 = m(i[7], 12140).wrapping_sub(m(i[8], 11003));
    let t10 = m(i[5], 8423).wrapping_add(m(i[10], 14053));
    let t11 = m(i[5], 14053).wrapping_sub(m(i[10], 8423));
    let t12 = m(i[3], 5520).wrapping_add(m(i[12], 15426));
    let t13 = m(i[3], 15426).wrapping_sub(m(i[12], 5520));
    let t14 = m(i[1], 2404).wrapping_add(m(i[14], 16207));
    let t15 = m(i[1], 16207).wrapping_sub(m(i[14], 2404));
    let t0a = r14(t0.wrapping_add(t8));
    let t1a = r14(t1.wrapping_add(t9));
    let t2a = r14(t2.wrapping_add(t10));
    let t3a = r14(t3.wrapping_add(t11));
    let t4a = r14(t4.wrapping_add(t12));
    let t5a = r14(t5.wrapping_add(t13));
    let t6a = r14(t6.wrapping_add(t14));
    let t7a = r14(t7.wrapping_add(t15));
    let t8a = r14(t0.wrapping_sub(t8));
    let t9a = r14(t1.wrapping_sub(t9));
    let t10a = r14(t2.wrapping_sub(t10));
    let t11a = r14(t3.wrapping_sub(t11));
    let t12a = r14(t4.wrapping_sub(t12));
    let t13a = r14(t5.wrapping_sub(t13));
    let t14a = r14(t6.wrapping_sub(t14));
    let t15a = r14(t7.wrapping_sub(t15));
    let t8 = m(t8a, 16069).wrapping_add(m(t9a, 3196));
    let t9 = m(t8a, 3196).wrapping_sub(m(t9a, 16069));
    let t10 = m(t10a, 9102).wrapping_add(m(t11a, 13623));
    let t11 = m(t10a, 13623).wrapping_sub(m(t11a, 9102));
    let t12 = m(t13a, 16069).wrapping_sub(m(t12a, 3196));
    let t13 = m(t13a, 3196).wrapping_add(m(t12a, 16069));
    let t14 = m(t15a, 9102).wrapping_sub(m(t14a, 13623));
    let t15 = m(t15a, 13623).wrapping_add(m(t14a, 9102));
    let t0 = t0a + t4a;
    let t1 = t1a + t5a;
    let t2 = t2a + t6a;
    let t3 = t3a + t7a;
    let t4 = t0a - t4a;
    let t5 = t1a - t5a;
    let t6 = t2a - t6a;
    let t7 = t3a - t7a;
    let t8a = r14(t8.wrapping_add(t12));
    let t9a = r14(t9.wrapping_add(t13));
    let t10a = r14(t10.wrapping_add(t14));
    let t11a = r14(t11.wrapping_add(t15));
    let t12a = r14(t8.wrapping_sub(t12));
    let t13a = r14(t9.wrapping_sub(t13));
    let t14a = r14(t10.wrapping_sub(t14));
    let t15a = r14(t11.wrapping_sub(t15));
    let t4a = m(t4, 15137).wrapping_add(m(t5, 6270));
    let t5a = m(t4, 6270).wrapping_sub(m(t5, 15137));
    let t6a = m(t7, 15137).wrapping_sub(m(t6, 6270));
    let t7a = m(t7, 6270).wrapping_add(m(t6, 15137));
    let t12 = m(t12a, 15137).wrapping_add(m(t13a, 6270));
    let t13 = m(t12a, 6270).wrapping_sub(m(t13a, 15137));
    let t14 = m(t15a, 15137).wrapping_sub(m(t14a, 6270));
    let t15 = m(t15a, 6270).wrapping_add(m(t14a, 15137));
    o[0] = t0 + t2;
    o[15] = -(t1 + t3);
    let t2a = t0 - t2;
    let t3a = t1 - t3;
    o[3] = -r14(t4a.wrapping_add(t6a));
    o[12] = r14(t5a.wrapping_add(t7a));
    let t6 = r14(t4a.wrapping_sub(t6a));
    let t7 = r14(t5a.wrapping_sub(t7a));
    o[1] = -(t8a + t10a);
    o[14] = t9a + t11a;
    let t10 = t8a - t10a;
    let t11 = t9a - t11a;
    o[2] = r14(t12.wrapping_add(t14));
    o[13] = -r14(t13.wrapping_add(t15));
    let t14a = r14(t12.wrapping_sub(t14));
    let t15a = r14(t13.wrapping_sub(t15));
    o[7] = r14(m(-(t2a + t3a), 11585));
    o[8] = r14(m(t2a - t3a, 11585));
    o[4] = r14(m(t7 + t6, 11585));
    o[11] = r14(m(t7 - t6, 11585));
    o[6] = r14(m(t11 + t10, 11585));
    o[9] = r14(m(t11 - t10, 11585));
    o[5] = r14(m(-(t14a + t15a), 11585));
    o[10] = r14(m(t14a - t15a, 11585));
}

fn idct32(i: &[i32], o: &mut [i32]) {
    let k = |a: i32, ca: i32, b: i32, cb: i32| r14(m(a, ca).wrapping_sub(m(b, cb)));
    let p = |a: i32, ca: i32, b: i32, cb: i32| r14(m(a, ca).wrapping_add(m(b, cb)));
    let t0a = r14(m(i[0] + i[16], 11585));
    let t1a = r14(m(i[0] - i[16], 11585));
    let t2a = k(i[8], 6270, i[24], 15137);
    let t3a = p(i[8], 15137, i[24], 6270);
    let t4a = k(i[4], 3196, i[28], 16069);
    let t7a = p(i[4], 16069, i[28], 3196);
    let t5a = k(i[20], 13623, i[12], 9102);
    let t6a = p(i[20], 9102, i[12], 13623);
    let t8a = k(i[2], 1606, i[30], 16305);
    let t15a = p(i[2], 16305, i[30], 1606);
    let t9a = k(i[18], 12665, i[14], 10394);
    let t14a = p(i[18], 10394, i[14], 12665);
    let t10a = k(i[10], 7723, i[22], 14449);
    let t13a = p(i[10], 14449, i[22], 7723);
    let t11a = k(i[26], 15679, i[6], 4756);
    let t12a = p(i[26], 4756, i[6], 15679);
    let t16a = k(i[1], 804, i[31], 16364);
    let t31a = p(i[1], 16364, i[31], 804);
    let t17a = k(i[17], 12140, i[15], 11003);
    let t30a = p(i[17], 11003, i[15], 12140);
    let t18a = k(i[9], 7005, i[23], 14811);
    let t29a = p(i[9], 14811, i[23], 7005);
    let t19a = k(i[25], 15426, i[7], 5520);
    let t28a = p(i[25], 5520, i[7], 15426);
    let t20a = k(i[5], 3981, i[27], 15893);
    let t27a = p(i[5], 15893, i[27], 3981);
    let t21a = k(i[21], 14053, i[11], 8423);
    let t26a = p(i[21], 8423, i[11], 14053);
    let t22a = k(i[13], 9760, i[19], 13160);
    let t25a = p(i[13], 13160, i[19], 9760);
    let t23a = k(i[29], 16207, i[3], 2404);
    let t24a = p(i[29], 2404, i[3], 16207);

    let t0 = t0a + t3a;
    let t1 = t1a + t2a;
    let t2 = t1a - t2a;
    let t3 = t0a - t3a;
    let t4 = t4a + t5a;
    let t5 = t4a - t5a;
    let t6 = t7a - t6a;
    let t7 = t7a + t6a;
    let t8 = t8a + t9a;
    let t9 = t8a - t9a;
    let t10 = t11a - t10a;
    let t11 = t11a + t10a;
    let t12 = t12a + t13a;
    let t13 = t12a - t13a;
    let t14 = t15a - t14a;
    let t15 = t15a + t14a;
    let t16 = t16a + t17a;
    let t17 = t16a - t17a;
    let t18 = t19a - t18a;
    let t19 = t19a + t18a;
    let t20 = t20a + t21a;
    let t21 = t20a - t21a;
    let t22 = t23a - t22a;
    let t23 = t23a + t22a;
    let t24 = t24a + t25a;
    let t25 = t24a - t25a;
    let t26 = t27a - t26a;
    let t27 = t27a + t26a;
    let t28 = t28a + t29a;
    let t29 = t28a - t29a;
    let t30 = t31a - t30a;
    let t31 = t31a + t30a;

    let n = |a: i32, ca: i32, b: i32, cb: i32| r14((m(a, ca).wrapping_add(m(b, cb))).wrapping_neg());
    let t5a = r14(m(t6 - t5, 11585));
    let t6a = r14(m(t6 + t5, 11585));
    let t9a = k(t14, 6270, t9, 15137);
    let t14a = p(t14, 15137, t9, 6270);
    let t10a = n(t13, 15137, t10, 6270);
    let t13a = k(t13, 6270, t10, 15137);
    let t17a = k(t30, 3196, t17, 16069);
    let t30a = p(t30, 16069, t17, 3196);
    let t18a = n(t29, 16069, t18, 3196);
    let t29a = k(t29, 3196, t18, 16069);
    let t21a = k(t26, 13623, t21, 9102);
    let t26a = p(t26, 9102, t21, 13623);
    let t22a = n(t25, 9102, t22, 13623);
    let t25a = k(t25, 13623, t22, 9102);

    let t0a = t0 + t7;
    let t1a = t1 + t6a;
    let t2a = t2 + t5a;
    let t3a = t3 + t4;
    let t4a = t3 - t4;
    let t5 = t2 - t5a;
    let t6 = t1 - t6a;
    let t7a = t0 - t7;
    let t8a = t8 + t11;
    let t9 = t9a + t10a;
    let t10 = t9a - t10a;
    let t11a = t8 - t11;
    let t12a = t15 - t12;
    let t13 = t14a - t13a;
    let t14 = t14a + t13a;
    let t15a = t15 + t12;
    let t16a = t16 + t19;
    let t17 = t17a + t18a;
    let t18 = t17a - t18a;
    let t19a = t16 - t19;
    let t20a = t23 - t20;
    let t21 = t22a - t21a;
    let t22 = t22a + t21a;
    let t23a = t23 + t20;
    let t24a = t24 + t27;
    let t25 = t25a + t26a;
    let t26 = t25a - t26a;
    let t27a = t24 - t27;
    let t28a = t31 - t28;
    let t29 = t30a - t29a;
    let t30 = t30a + t29a;
    let t31a = t31 + t28;

    let t10a = r14(m(t13 - t10, 11585));
    let t13a = r14(m(t13 + t10, 11585));
    let t11 = r14(m(t12a - t11a, 11585));
    let t12 = r14(m(t12a + t11a, 11585));
    let t18a = k(t29, 6270, t18, 15137);
    let t29a = p(t29, 15137, t18, 6270);
    let t19 = k(t28a, 6270, t19a, 15137);
    let t28 = p(t28a, 15137, t19a, 6270);
    let t20 = n(t27a, 15137, t20a, 6270);
    let t27 = k(t27a, 6270, t20a, 15137);
    let t21a = n(t26, 15137, t21, 6270);
    let t26a = k(t26, 6270, t21, 15137);

    let t0 = t0a + t15a;
    let t1 = t1a + t14;
    let t2 = t2a + t13a;
    let t3 = t3a + t12;
    let t4 = t4a + t11;
    let t5a = t5 + t10a;
    let t6a = t6 + t9;
    let t7 = t7a + t8a;
    let t8 = t7a - t8a;
    let t9a = t6 - t9;
    let t10 = t5 - t10a;
    let t11a = t4a - t11;
    let t12a = t3a - t12;
    let t13 = t2a - t13a;
    let t14a = t1a - t14;
    let t15 = t0a - t15a;
    let t16 = t16a + t23a;
    let t17a = t17 + t22;
    let t18 = t18a + t21a;
    let t19a = t19 + t20;
    let t20a = t19 - t20;
    let t21 = t18a - t21a;
    let t22a = t17 - t22;
    let t23 = t16a - t23a;
    let t24 = t31a - t24a;
    let t25a = t30 - t25;
    let t26 = t29a - t26a;
    let t27a = t28 - t27;
    let t28a = t28 + t27;
    let t29 = t29a + t26a;
    let t30a = t30 + t25;
    let t31 = t31a + t24a;

    let t20 = r14(m(t27a - t20a, 11585));
    let t27 = r14(m(t27a + t20a, 11585));
    let t21a = r14(m(t26 - t21, 11585));
    let t26a = r14(m(t26 + t21, 11585));
    let t22 = r14(m(t25a - t22a, 11585));
    let t25 = r14(m(t25a + t22a, 11585));
    let t23a = r14(m(t24 - t23, 11585));
    let t24a = r14(m(t24 + t23, 11585));

    o[0] = t0 + t31;
    o[1] = t1 + t30a;
    o[2] = t2 + t29;
    o[3] = t3 + t28a;
    o[4] = t4 + t27;
    o[5] = t5a + t26a;
    o[6] = t6a + t25;
    o[7] = t7 + t24a;
    o[8] = t8 + t23a;
    o[9] = t9a + t22;
    o[10] = t10 + t21a;
    o[11] = t11a + t20;
    o[12] = t12a + t19a;
    o[13] = t13 + t18;
    o[14] = t14a + t17a;
    o[15] = t15 + t16;
    o[16] = t15 - t16;
    o[17] = t14a - t17a;
    o[18] = t13 - t18;
    o[19] = t12a - t19a;
    o[20] = t11a - t20;
    o[21] = t10 - t21a;
    o[22] = t9a - t22;
    o[23] = t8 - t23a;
    o[24] = t7 - t24a;
    o[25] = t6a - t25;
    o[26] = t5a - t26a;
    o[27] = t4 - t27;
    o[28] = t3 - t28a;
    o[29] = t2 - t29;
    o[30] = t1 - t30a;
    o[31] = t0 - t31;
}

fn iwht4(i: &[i32], o: &mut [i32], pass: usize) {
    let (mut t0, mut t1, mut t2, mut t3) = if pass == 0 { (i[0] >> 2, i[3] >> 2, i[1] >> 2, i[2] >> 2) } else { (i[0], i[3], i[1], i[2]) };
    t0 += t2;
    t3 -= t1;
    let t4 = (t0 - t3) >> 1;
    t1 = t4 - t1;
    t2 = t4 - t2;
    t0 -= t1;
    t3 += t2;
    o[0] = t0;
    o[1] = t1;
    o[2] = t2;
    o[3] = t3;
}

fn tx1d(kind: u8, sz: usize, i: &[i32], o: &mut [i32], pass: usize) {
    match (kind, sz) {
        (0, 4) => idct4(i, o),
        (1, 4) => iadst4(i, o),
        (0, 8) => idct8(i, o),
        (1, 8) => iadst8(i, o),
        (0, 16) => idct16(i, o),
        (1, 16) => iadst16(i, o),
        (0, 32) => idct32(i, o),
        _ => iwht4(i, o, pass),
    }
}

/// Inverse transform `block` (cleared afterwards) and add it at `o`. `tx`: 0..3 sizes,
/// 4 = lossless WHT. `txtp`: 0 DCT_DCT, 1 DCT_ADST (ADST first pass), 2 ADST_DCT, 3 ADST_ADST.
pub fn itxfm_add(tx: usize, txtp: usize, d: &mut [u8], o: usize, stride: usize, block: &mut [i16], eob: usize) {
    let (sz, bits, ka, kb) = match tx {
        4 => (4, 0, 2u8, 2u8),
        3 => (32, 6, 0, 0),
        _ => {
            let sz = 4 << tx;
            let bits = [4, 5, 6][tx];
            let (ka, kb) = match txtp {
                1 => (1, 0),
                2 => (0, 1),
                3 => (1, 1),
                _ => (0, 0),
            };
            (sz, bits, ka, kb)
        }
    };
    let rnd = |v: i32| if bits > 0 { (v + (1 << (bits - 1))) >> bits } else { v };
    if tx != 4 && ka == 0 && kb == 0 && eob == 1 {
        let t = r14(m(r14(m(block[0] as i32, 11585)), 11585));
        block[0] = 0;
        let add = rnd(t);
        for y in 0..sz {
            let r = &mut d[o + y * stride..o + y * stride + sz];
            for p in r.iter_mut() {
                *p = clip(*p as i32 + add);
            }
        }
        return;
    }
    let mut tmp = [0i16; 32 * 32];
    let mut inp = [0i32; 32];
    let mut out = [0i32; 32];
    for i in 0..sz {
        for x in 0..sz {
            inp[x] = block[i + x * sz] as i32;
        }
        tx1d(ka, sz, &inp, &mut out, 0);
        for k in 0..sz {
            tmp[i * sz + k] = out[k] as i16;
        }
    }
    block[..sz * sz].iter_mut().for_each(|v| *v = 0);
    for i in 0..sz {
        for x in 0..sz {
            inp[x] = tmp[i + x * sz] as i32;
        }
        tx1d(kb, sz, &inp, &mut out, 1);
        for j in 0..sz {
            let v = out[j] as i16 as i32;
            let p = &mut d[o + j * stride + i];
            *p = clip(*p as i32 + rnd(v));
        }
    }
}

// ---- Loop filter ----

/// Filter 8 positions along an edge. `sa` steps along the edge, `sb` across it.
#[allow(clippy::too_many_arguments)]
pub fn loop_filter(d: &mut [u8], at: usize, e: i32, i: i32, h: i32, sa: isize, sb: isize, wd: u32) {
    let f = 1;
    for k in 0..8isize {
        let base = at as isize + k * sa;
        let px = |n: isize| d[(base + n * sb) as usize] as i32;
        let (p3, p2, p1, p0) = (px(-4), px(-3), px(-2), px(-1));
        let (q0, q1, q2, q3) = (px(0), px(1), px(2), px(3));
        let fm = (p3 - p2).abs() <= i
            && (p2 - p1).abs() <= i
            && (p1 - p0).abs() <= i
            && (q1 - q0).abs() <= i
            && (q2 - q1).abs() <= i
            && (q3 - q2).abs() <= i
            && (p0 - q0).abs() * 2 + ((p1 - q1).abs() >> 1) <= e;
        if !fm {
            continue;
        }
        let flat8in = wd >= 8 && (p3 - p0).abs() <= f && (p2 - p0).abs() <= f && (p1 - p0).abs() <= f && (q1 - q0).abs() <= f && (q2 - q0).abs() <= f && (q3 - q0).abs() <= f;
        let mut flat8out = false;
        let (mut p7, mut p6, mut p5, mut p4, mut q4, mut q5, mut q6, mut q7) = (0, 0, 0, 0, 0, 0, 0, 0);
        if wd >= 16 && flat8in {
            let px = |n: isize| d[(base + n * sb) as usize] as i32;
            p7 = px(-8);
            p6 = px(-7);
            p5 = px(-6);
            p4 = px(-5);
            q4 = px(4);
            q5 = px(5);
            q6 = px(6);
            q7 = px(7);
            flat8out = (p7 - p0).abs() <= f && (p6 - p0).abs() <= f && (p5 - p0).abs() <= f && (p4 - p0).abs() <= f && (q4 - q0).abs() <= f && (q5 - q0).abs() <= f && (q6 - q0).abs() <= f && (q7 - q0).abs() <= f;
        }
        let mut set = |n: isize, v: i32| d[(base + n * sb) as usize] = v as u8;
        if wd >= 16 && flat8out && flat8in {
            set(-7, (p7 * 7 + p6 * 2 + p5 + p4 + p3 + p2 + p1 + p0 + q0 + 8) >> 4);
            set(-6, (p7 * 6 + p6 + p5 * 2 + p4 + p3 + p2 + p1 + p0 + q0 + q1 + 8) >> 4);
            set(-5, (p7 * 5 + p6 + p5 + p4 * 2 + p3 + p2 + p1 + p0 + q0 + q1 + q2 + 8) >> 4);
            set(-4, (p7 * 4 + p6 + p5 + p4 + p3 * 2 + p2 + p1 + p0 + q0 + q1 + q2 + q3 + 8) >> 4);
            set(-3, (p7 * 3 + p6 + p5 + p4 + p3 + p2 * 2 + p1 + p0 + q0 + q1 + q2 + q3 + q4 + 8) >> 4);
            set(-2, (p7 * 2 + p6 + p5 + p4 + p3 + p2 + p1 * 2 + p0 + q0 + q1 + q2 + q3 + q4 + q5 + 8) >> 4);
            set(-1, (p7 + p6 + p5 + p4 + p3 + p2 + p1 + p0 * 2 + q0 + q1 + q2 + q3 + q4 + q5 + q6 + 8) >> 4);
            set(0, (p6 + p5 + p4 + p3 + p2 + p1 + p0 + q0 * 2 + q1 + q2 + q3 + q4 + q5 + q6 + q7 + 8) >> 4);
            set(1, (p5 + p4 + p3 + p2 + p1 + p0 + q0 + q1 * 2 + q2 + q3 + q4 + q5 + q6 + q7 * 2 + 8) >> 4);
            set(2, (p4 + p3 + p2 + p1 + p0 + q0 + q1 + q2 * 2 + q3 + q4 + q5 + q6 + q7 * 3 + 8) >> 4);
            set(3, (p3 + p2 + p1 + p0 + q0 + q1 + q2 + q3 * 2 + q4 + q5 + q6 + q7 * 4 + 8) >> 4);
            set(4, (p2 + p1 + p0 + q0 + q1 + q2 + q3 + q4 * 2 + q5 + q6 + q7 * 5 + 8) >> 4);
            set(5, (p1 + p0 + q0 + q1 + q2 + q3 + q4 + q5 * 2 + q6 + q7 * 6 + 8) >> 4);
            set(6, (p0 + q0 + q1 + q2 + q3 + q4 + q5 + q6 * 2 + q7 * 7 + 8) >> 4);
        } else if wd >= 8 && flat8in {
            set(-3, (p3 * 3 + 2 * p2 + p1 + p0 + q0 + 4) >> 3);
            set(-2, (p3 * 2 + p2 + 2 * p1 + p0 + q0 + q1 + 4) >> 3);
            set(-1, (p3 + p2 + p1 + 2 * p0 + q0 + q1 + q2 + 4) >> 3);
            set(0, (p2 + p1 + p0 + 2 * q0 + q1 + q2 + q3 + 4) >> 3);
            set(1, (p1 + p0 + q0 + 2 * q1 + q2 + q3 * 2 + 4) >> 3);
            set(2, (p0 + q0 + q1 + 2 * q2 + q3 * 3 + 4) >> 3);
        } else {
            let hev = (p1 - p0).abs() > h || (q1 - q0).abs() > h;
            if hev {
                let f = (p1 - q1).clamp(-128, 127);
                let f = (3 * (q0 - p0) + f).clamp(-128, 127);
                let f1 = (f + 4).min(127) >> 3;
                let f2 = (f + 3).min(127) >> 3;
                set(-1, (p0 + f2).clamp(0, 255));
                set(0, (q0 - f1).clamp(0, 255));
            } else {
                let f = (3 * (q0 - p0)).clamp(-128, 127);
                let f1 = (f + 4).min(127) >> 3;
                let f2 = (f + 3).min(127) >> 3;
                set(-1, (p0 + f2).clamp(0, 255));
                set(0, (q0 - f1).clamp(0, 255));
                let f = (f1 + 1) >> 1;
                set(-2, (p1 + f).clamp(0, 255));
                set(1, (q1 - f).clamp(0, 255));
            }
        }
    }
}
