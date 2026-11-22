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
