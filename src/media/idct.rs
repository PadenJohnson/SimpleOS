//! The 8x8 inverse DCT used by MPEG-1/2 and MPEG-4 Part 2 pictures. Integer arithmetic in the
//! same steps as FFmpeg's "simple" IDCT, so pictures match FFmpeg's exactly (the standards
//! only bound the error, and small differences would add up from picture to picture).

const W1: i32 = 22725;
const W2: i32 = 21407;
const W3: i32 = 19266;
const W4: i32 = 16383;
const W5: i32 = 12873;
const W6: i32 = 8867;
const W7: i32 = 4520;
const ROW_SHIFT: u32 = 11;
const COL_SHIFT: u32 = 20;

#[inline]
fn row(r: &mut [i16]) {
    if r[1] == 0 && r[2] == 0 && r[3] == 0 && r[4] == 0 && r[5] == 0 && r[6] == 0 && r[7] == 0 {
        let v = ((r[0] as i32) << 3) as i16;
        for x in r.iter_mut().take(8) {
            *x = v;
        }
        return;
    }
    let (r0, r1, r2, r3) = (r[0] as i32, r[1] as i32, r[2] as i32, r[3] as i32);
    let mut a0 = W4 * r0 + (1 << (ROW_SHIFT - 1));
    let mut a1 = a0;
    let mut a2 = a0;
    let mut a3 = a0;
    a0 += W2 * r2;
    a1 += W6 * r2;
    a2 -= W6 * r2;
    a3 -= W2 * r2;
    let mut b0 = W1 * r1 + W3 * r3;
    let mut b1 = W3 * r1 - W7 * r3;
    let mut b2 = W5 * r1 - W1 * r3;
    let mut b3 = W7 * r1 - W5 * r3;
    let (r4, r5, r6, r7) = (r[4] as i32, r[5] as i32, r[6] as i32, r[7] as i32);
    if r4 != 0 || r5 != 0 || r6 != 0 || r7 != 0 {
        a0 += W4 * r4 + W6 * r6;
        a1 += -W4 * r4 - W2 * r6;
        a2 += -W4 * r4 + W2 * r6;
        a3 += W4 * r4 - W6 * r6;
        b0 += W5 * r5 + W7 * r7;
        b1 += -W1 * r5 - W5 * r7;
        b2 += W7 * r5 + W3 * r7;
        b3 += W3 * r5 - W1 * r7;
    }
    r[0] = ((a0 + b0) >> ROW_SHIFT) as i16;
    r[7] = ((a0 - b0) >> ROW_SHIFT) as i16;
    r[1] = ((a1 + b1) >> ROW_SHIFT) as i16;
    r[6] = ((a1 - b1) >> ROW_SHIFT) as i16;
    r[2] = ((a2 + b2) >> ROW_SHIFT) as i16;
    r[5] = ((a2 - b2) >> ROW_SHIFT) as i16;
    r[3] = ((a3 + b3) >> ROW_SHIFT) as i16;
    r[4] = ((a3 - b3) >> ROW_SHIFT) as i16;
}

/// One column: the eight output values.
#[inline]
fn col(b: &[i16; 64], c: usize) -> [i32; 8] {
    let g = |k: usize| b[k * 8 + c] as i32;
    let mut a0 = W4 * (g(0) + ((1 << (COL_SHIFT - 1)) / W4));
    let mut a1 = a0;
    let mut a2 = a0;
    let mut a3 = a0;
    a0 += W2 * g(2);
    a1 += W6 * g(2);
    a2 += -W6 * g(2);
    a3 += -W2 * g(2);
    let mut b0 = W1 * g(1) + W3 * g(3);
    let mut b1 = W3 * g(1) - W7 * g(3);
    let mut b2 = W5 * g(1) - W1 * g(3);
    let mut b3 = W7 * g(1) - W5 * g(3);
    if g(4) != 0 {
        a0 += W4 * g(4);
        a1 += -W4 * g(4);
        a2 += -W4 * g(4);
        a3 += W4 * g(4);
    }
    if g(5) != 0 {
        b0 += W5 * g(5);
        b1 += -W1 * g(5);
        b2 += W7 * g(5);
        b3 += W3 * g(5);
    }
    if g(6) != 0 {
        a0 += W6 * g(6);
        a1 += -W2 * g(6);
        a2 += W2 * g(6);
        a3 += -W6 * g(6);
    }
    if g(7) != 0 {
        b0 += W7 * g(7);
        b1 += -W5 * g(7);
        b2 += W3 * g(7);
        b3 += -W1 * g(7);
    }
    [
        (a0 + b0) >> COL_SHIFT,
        (a1 + b1) >> COL_SHIFT,
        (a2 + b2) >> COL_SHIFT,
        (a3 + b3) >> COL_SHIFT,
        (a3 - b3) >> COL_SHIFT,
        (a2 - b2) >> COL_SHIFT,
        (a1 - b1) >> COL_SHIFT,
        (a0 - b0) >> COL_SHIFT,
    ]
}

fn rows(b: &mut [i16; 64]) {
    for r in b.chunks_exact_mut(8) {
        row(r);
    }
}

#[inline]
fn clip(v: i32) -> u8 {
    v.clamp(0, 255) as u8
}

/// Transform and write the block to `dst` (8x8 at `at`, rows `stride` apart).
pub fn put(b: &mut [i16; 64], dst: &mut [u8], at: usize, stride: usize) {
    rows(b);
    for c in 0..8 {
        let v = col(b, c);
        for (k, x) in v.iter().enumerate() {
            dst[at + k * stride + c] = clip(*x);
        }
    }
}

/// Transform and add the block to the prediction already in `dst`.
pub fn add(b: &mut [i16; 64], dst: &mut [u8], at: usize, stride: usize) {
    rows(b);
    for c in 0..8 {
        let v = col(b, c);
        for (k, x) in v.iter().enumerate() {
            let p = &mut dst[at + k * stride + c];
            *p = clip(*p as i32 + *x);
        }
    }
}

// ---- Xvid's IDCT ----
// Xvid reconstructs its own pictures with this IDCT, so files it made decode exactly (and
// don't drift over long key-frame intervals) only with the same one. Same steps as FFmpeg's
// xvididct.c.

const XROW_SHIFT: u32 = 11;
const XCOL_SHIFT: u32 = 6;
const TAB04: [i32; 7] = [22725, 21407, 19266, 16384, 12873, 8867, 4520];
const TAB17: [i32; 7] = [31521, 29692, 26722, 22725, 17855, 12299, 6270];
const TAB26: [i32; 7] = [29692, 27969, 25172, 21407, 16819, 11585, 5906];
const TAB35: [i32; 7] = [26722, 25172, 22654, 19266, 15137, 10426, 5315];

fn xrow(r: &mut [i16], t: &[i32; 7], rnd: i32) {
    let x = |i: usize| r[i] as i32;
    let (c1, c2, c3, c4, c5, c6, c7) = (t[0], t[1], t[2], t[3], t[4], t[5], t[6]);
    let m = |c: i32, v: i32| c.wrapping_mul(v);
    let k = m(c4, x(0)).wrapping_add(rnd);
    // Only a DC value: the row is that value (the rounding is folded in, so a zero row of the
    // first three becomes a small constant); if it comes out 0 the row is left as it is.
    if r[1..8].iter().all(|&v| v == 0) {
        let a0 = k >> XROW_SHIFT;
        if a0 != 0 {
            for v in r.iter_mut().take(8) {
                *v = a0 as i16;
            }
        }
        return;
    }
    let a0 = k.wrapping_add(m(c2, x(2))).wrapping_add(m(c4, x(4))).wrapping_add(m(c6, x(6)));
    let a1 = k.wrapping_add(m(c6, x(2))).wrapping_sub(m(c4, x(4))).wrapping_sub(m(c2, x(6)));
    let a2 = k.wrapping_sub(m(c6, x(2))).wrapping_sub(m(c4, x(4))).wrapping_add(m(c2, x(6)));
    let a3 = k.wrapping_sub(m(c2, x(2))).wrapping_add(m(c4, x(4))).wrapping_sub(m(c6, x(6)));
    let b0 = m(c1, x(1)).wrapping_add(m(c3, x(3))).wrapping_add(m(c5, x(5))).wrapping_add(m(c7, x(7)));
    let b1 = m(c3, x(1)).wrapping_sub(m(c7, x(3))).wrapping_sub(m(c1, x(5))).wrapping_sub(m(c5, x(7)));
    let b2 = m(c5, x(1)).wrapping_sub(m(c1, x(3))).wrapping_add(m(c7, x(5))).wrapping_add(m(c3, x(7)));
    let b3 = m(c7, x(1)).wrapping_sub(m(c5, x(3))).wrapping_add(m(c3, x(5))).wrapping_sub(m(c1, x(7)));
    r[0] = (a0.wrapping_add(b0) >> XROW_SHIFT) as i16;
    r[1] = (a1.wrapping_add(b1) >> XROW_SHIFT) as i16;
    r[2] = (a2.wrapping_add(b2) >> XROW_SHIFT) as i16;
    r[3] = (a3.wrapping_add(b3) >> XROW_SHIFT) as i16;
    r[4] = (a3.wrapping_sub(b3) >> XROW_SHIFT) as i16;
    r[5] = (a2.wrapping_sub(b2) >> XROW_SHIFT) as i16;
    r[6] = (a1.wrapping_sub(b1) >> XROW_SHIFT) as i16;
    r[7] = (a0.wrapping_sub(b0) >> XROW_SHIFT) as i16;
}

fn xcol(b: &mut [i16; 64], c: usize) {
    const TAN1: i32 = 0x32EC;
    const TAN2: i32 = 0x6A0A;
    const TAN3: i32 = 0xAB0E;
    const SQRT2: i32 = 0x5A82;
    let mult = |k: i32, x: i32| k.wrapping_mul(x) >> 16;
    let g = |k: usize| b[k * 8 + c] as i32;
    // Odd part.
    let (m4, m5, m6, m7) = (g(7), g(5), g(3), g(1));
    let m0 = mult(TAN1, m4) + m7;
    let m1 = mult(TAN1, m7) - m4;
    let m2 = mult(TAN3, m5) + m6;
    let m3 = mult(TAN3, m6) - m5;
    let m7 = m0 + m2;
    let m4 = m1 - m3;
    let m0 = m0 - m2;
    let m1 = m1 + m3;
    let m6 = 2 * mult(SQRT2, m0 + m1);
    let m5 = 2 * mult(SQRT2, m0 - m1);
    // Even part.
    let (e1, e2) = (g(2), g(6));
    let e3 = mult(TAN2, e2) + e1;
    let e2 = mult(TAN2, e1) - e2;
    let e0 = g(0) + g(4);
    let e1 = g(0) - g(4);
    let (e0, e3) = (e0 + e3, e0 - e3);
    let (e0, m7) = (e0 + m7, e0 - m7);
    let (e3, m4) = (e3 + m4, e3 - m4);
    let (e1, e2) = (e1 + e2, e1 - e2);
    let (e1, m6) = (e1 + m6, e1 - m6);
    let (e2, m5) = (e2 + m5, e2 - m5);
    let out = [e0, e1, e2, e3, m4, m5, m6, m7];
    for (k, v) in out.iter().enumerate() {
        b[k * 8 + c] = (v >> XCOL_SHIFT) as i16;
    }
}

fn xvid(b: &mut [i16; 64]) {
    const TABS: [(&[i32; 7], i32); 8] = [(&TAB04, 65536), (&TAB17, 3597), (&TAB26, 2260), (&TAB35, 1203), (&TAB04, 0), (&TAB35, 120), (&TAB26, 512), (&TAB17, 512)];
    for (k, r) in b.chunks_exact_mut(8).enumerate() {
        xrow(r, TABS[k].0, TABS[k].1);
    }
    for c in 0..8 {
        xcol(b, c);
    }
}

pub fn put_xvid(b: &mut [i16; 64], dst: &mut [u8], at: usize, stride: usize) {
    xvid(b);
    for r in 0..8 {
        for c in 0..8 {
            dst[at + r * stride + c] = clip(b[r * 8 + c] as i32);
        }
    }
}

pub fn add_xvid(b: &mut [i16; 64], dst: &mut [u8], at: usize, stride: usize) {
    xvid(b);
    for r in 0..8 {
        for c in 0..8 {
            let p = &mut dst[at + r * stride + c];
            *p = clip(*p as i32 + b[r * 8 + c] as i32);
        }
    }
}
