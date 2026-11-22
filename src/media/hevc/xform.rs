//! HEVC inverse transforms (DCT 4..32, the 4x4 DST for intra luma, transform skip) and
//! adding the residual to the prediction.

use super::tab::TRANSFORM;

const DST: [i32; 16] = [29, 55, 74, 84, 74, 74, 0, -74, 84, -29, -74, 55, 55, -84, 74, -29];

/// In-place inverse transform of an n x n block (raster order) to residuals.
pub fn inverse(c: &mut [i32], log2: u32, dst: bool, bit_depth: u32) {
    let n = 1usize << log2;
    // The n-point matrix: row k is row k * 32 / n of the 32-point one.
    let mut m = [0i32; 32 * 32];
    if dst {
        m[..16].copy_from_slice(&DST);
    } else {
        let step = 32 / n;
        for k in 0..n {
            for j in 0..n {
                m[k * n + j] = TRANSFORM[k * step * 32 + j] as i32;
            }
        }
    }
    // Rows that have any nonzero coefficient.
    let mut last_row = 0;
    for k in 0..n {
        if c[k * n..k * n + n].iter().any(|&v| v != 0) {
            last_row = k + 1;
        }
    }
    let mut tmp = [0i32; 32 * 32];
    // Columns: tmp[y][x] = sum_k m[k][y] * c[k][x].
    for k in 0..last_row {
        let crow = &c[k * n..k * n + n];
        let mrow = &m[k * n..k * n + n];
        for (y, &mk) in mrow.iter().enumerate() {
            if mk == 0 {
                continue;
            }
            let t = &mut tmp[y * n..y * n + n];
            for x in 0..n {
                t[x] += mk * crow[x];
            }
        }
    }
    for v in tmp.iter_mut().take(n * n) {
        *v = ((*v + 64) >> 7).clamp(-32768, 32767);
    }
    let shift = 20 - bit_depth;
    let add = 1i32 << (shift - 1);
    for y in 0..n {
        let row = &tmp[y * n..y * n + n];
        let mut last = 0;
        for (k, &v) in row.iter().enumerate() {
            if v != 0 {
                last = k + 1;
            }
        }
        let out = &mut c[y * n..y * n + n];
        for v in out.iter_mut() {
            *v = 0;
        }
        for k in 0..last {
            let rk = row[k];
            if rk == 0 {
                continue;
            }
            let mrow = &m[k * n..k * n + n];
            for x in 0..n {
                out[x] += mrow[x] * rk;
            }
        }
        for v in out.iter_mut() {
            *v = (*v + add) >> shift;
        }
    }
}

/// Transform skip: scale the coefficients straight to residuals.
pub fn skip(c: &mut [i32], log2: u32, bit_depth: u32) {
    let n = 1usize << log2;
    let shift = 15 - bit_depth as i32 - log2 as i32;
    for v in c.iter_mut().take(n * n) {
        *v = if shift > 0 { (*v + (1 << (shift - 1))) >> shift } else { *v << -shift };
    }
}

/// Add residuals to the block at `at` (rows `stride` apart).
pub fn add(dst: &mut [u16], at: usize, stride: usize, r: &[i32], log2: u32, bit_depth: u32) {
    let n = 1usize << log2;
    let max = (1i32 << bit_depth) - 1;
    for y in 0..n {
        let d = &mut dst[at + y * stride..at + y * stride + n];
        let rr = &r[y * n..y * n + n];
        for x in 0..n {
            d[x] = (d[x] as i32 + rr[x]).clamp(0, max) as u16;
        }
    }
}
