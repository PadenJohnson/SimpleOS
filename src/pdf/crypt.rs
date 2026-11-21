//! Opening encrypted PDFs that have no "open" password (very common: the owner password only
//! limits printing/editing in other programs). The standard security handler: RC4 (40/128-bit)
//! and AES-128/256, with MD5, SHA-256/384/512 written out here.

use alloc::vec;
use alloc::vec::Vec;

// ---- MD5 ------------------------------------------------------------------------------------

pub fn md5(data: &[u8]) -> [u8; 16] {
    const S: [u32; 64] = [7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 5, 9, 14, 20, 5, 9, 14, 20, 5, 9, 14, 20, 5, 9, 14, 20, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 6, 10, 15, 21, 6, 10, 15, 21, 6, 10, 15, 21, 6, 10, 15, 21];
    const K: [u32; 64] = [
        0xd76aa478, 0xe8c7b756, 0x242070db, 0xc1bdceee, 0xf57c0faf, 0x4787c62a, 0xa8304613, 0xfd469501, 0x698098d8, 0x8b44f7af, 0xffff5bb1, 0x895cd7be, 0x6b901122, 0xfd987193, 0xa679438e, 0x49b40821, 0xf61e2562, 0xc040b340, 0x265e5a51, 0xe9b6c7aa, 0xd62f105d, 0x02441453, 0xd8a1e681, 0xe7d3fbc8, 0x21e1cde6, 0xc33707d6, 0xf4d50d87, 0x455a14ed, 0xa9e3e905, 0xfcefa3f8, 0x676f02d9, 0x8d2a4c8a, 0xfffa3942, 0x8771f681, 0x6d9d6122, 0xfde5380c, 0xa4beea44, 0x4bdecfa9, 0xf6bb4b60, 0xbebfbc70, 0x289b7ec6, 0xeaa127fa, 0xd4ef3085, 0x04881d05, 0xd9d4d039, 0xe6db99e5, 0x1fa27cf8, 0xc4ac5665, 0xf4292244, 0x432aff97, 0xab9423a7, 0xfc93a039, 0x655b59c3, 0x8f0ccc92, 0xffeff47d, 0x85845dd1, 0x6fa87e4f, 0xfe2ce6e0, 0xa3014314, 0x4e0811a1, 0xf7537e82, 0xbd3af235, 0x2ad7d2bb, 0xeb86d391,
    ];
    let mut h = [0x67452301u32, 0xefcdab89, 0x98badcfe, 0x10325476];
    let mut msg = data.to_vec();
    let bits = (data.len() as u64).wrapping_mul(8);
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&bits.to_le_bytes());
    for chunk in msg.chunks(64) {
        let m: Vec<u32> = chunk.chunks(4).map(|w| u32::from_le_bytes([w[0], w[1], w[2], w[3]])).collect();
        let (mut a, mut b, mut c, mut d) = (h[0], h[1], h[2], h[3]);
        for i in 0..64 {
            let (f, g) = match i / 16 {
                0 => ((b & c) | (!b & d), i),
                1 => ((d & b) | (!d & c), (5 * i + 1) % 16),
                2 => (b ^ c ^ d, (3 * i + 5) % 16),
                _ => (c ^ (b | !d), (7 * i) % 16),
            };
            let f = f.wrapping_add(a).wrapping_add(K[i]).wrapping_add(m[g]);
            a = d;
            d = c;
            c = b;
            b = b.wrapping_add(f.rotate_left(S[i]));
        }
        h[0] = h[0].wrapping_add(a);
        h[1] = h[1].wrapping_add(b);
        h[2] = h[2].wrapping_add(c);
        h[3] = h[3].wrapping_add(d);
    }
    let mut out = [0u8; 16];
    for i in 0..4 {
        out[i * 4..i * 4 + 4].copy_from_slice(&h[i].to_le_bytes());
    }
    out
}

// ---- RC4 ------------------------------------------------------------------------------------

pub fn rc4(key: &[u8], data: &[u8]) -> Vec<u8> {
    let mut s: [u8; 256] = core::array::from_fn(|i| i as u8);
    let mut j = 0u8;
    for i in 0..256 {
        j = j.wrapping_add(s[i]).wrapping_add(key[i % key.len().max(1)]);
        s.swap(i, j as usize);
    }
    let (mut i, mut j) = (0u8, 0u8);
    data.iter()
        .map(|&b| {
            i = i.wrapping_add(1);
            j = j.wrapping_add(s[i as usize]);
            s.swap(i as usize, j as usize);
            b ^ s[s[i as usize].wrapping_add(s[j as usize]) as usize]
        })
        .collect()
}

// ---- AES (decryption, and encryption of single blocks for R6 hashing) --------------------

const SBOX: [u8; 256] = {
    let mut s = [0u8; 256];
    let mut p: u8 = 1;
    let mut q: u8 = 1;
    loop {
        p = p ^ (p << 1) ^ if p & 0x80 != 0 { 0x1B } else { 0 };
        q ^= q << 1;
        q ^= q << 2;
        q ^= q << 4;
        if q & 0x80 != 0 {
            q ^= 0x09;
        }
        let x = q ^ q.rotate_left(1) ^ q.rotate_left(2) ^ q.rotate_left(3) ^ q.rotate_left(4);
        s[p as usize] = x ^ 0x63;
        if p == 1 {
            break;
        }
    }
    s[0] = 0x63;
    s
};

const INV_SBOX: [u8; 256] = {
    let mut inv = [0u8; 256];
    let mut i = 0;
    while i < 256 {
        inv[SBOX[i] as usize] = i as u8;
        i += 1;
    }
    inv
};

fn xtime(x: u8) -> u8 {
    (x << 1) ^ if x & 0x80 != 0 { 0x1B } else { 0 }
}
fn gmul(mut a: u8, mut b: u8) -> u8 {
    let mut p = 0;
    while b != 0 {
        if b & 1 != 0 {
            p ^= a;
        }
        a = xtime(a);
        b >>= 1;
    }
    p
}

struct Aes {
    rk: Vec<[u8; 16]>,
}

impl Aes {
    fn new(key: &[u8]) -> Aes {
        let nk = key.len() / 4;
        let nr = nk + 6;
        let mut w: Vec<[u8; 4]> = key.chunks(4).map(|c| [c[0], c[1], c[2], c[3]]).collect();
        let mut rcon = 1u8;
        for i in nk..4 * (nr + 1) {
            let mut t = w[i - 1];
            if i % nk == 0 {
                t = [SBOX[t[1] as usize] ^ rcon, SBOX[t[2] as usize], SBOX[t[3] as usize], SBOX[t[0] as usize]];
                rcon = xtime(rcon);
            } else if nk > 6 && i % nk == 4 {
                t = [SBOX[t[0] as usize], SBOX[t[1] as usize], SBOX[t[2] as usize], SBOX[t[3] as usize]];
            }
            let p = w[i - nk];
            w.push([p[0] ^ t[0], p[1] ^ t[1], p[2] ^ t[2], p[3] ^ t[3]]);
        }
        let rk = w.chunks(4).map(|c| core::array::from_fn(|k| c[k / 4][k % 4])).collect();
        Aes { rk }
    }

    fn encrypt(&self, b: &mut [u8; 16]) {
        let nr = self.rk.len() - 1;
        for k in 0..16 {
            b[k] ^= self.rk[0][k];
        }
        for r in 1..=nr {
            for x in b.iter_mut() {
                *x = SBOX[*x as usize];
            }
            let s = *b;
            for c in 0..4 {
                for row in 0..4 {
                    b[c * 4 + row] = s[((c + row) % 4) * 4 + row];
                }
            }
            if r != nr {
                for c in 0..4 {
                    let a = [b[c * 4], b[c * 4 + 1], b[c * 4 + 2], b[c * 4 + 3]];
                    b[c * 4] = gmul(a[0], 2) ^ gmul(a[1], 3) ^ a[2] ^ a[3];
                    b[c * 4 + 1] = a[0] ^ gmul(a[1], 2) ^ gmul(a[2], 3) ^ a[3];
                    b[c * 4 + 2] = a[0] ^ a[1] ^ gmul(a[2], 2) ^ gmul(a[3], 3);
                    b[c * 4 + 3] = gmul(a[0], 3) ^ a[1] ^ a[2] ^ gmul(a[3], 2);
                }
            }
            for k in 0..16 {
                b[k] ^= self.rk[r][k];
            }
        }
    }

    fn decrypt(&self, b: &mut [u8; 16]) {
        let nr = self.rk.len() - 1;
        for k in 0..16 {
            b[k] ^= self.rk[nr][k];
        }
        for r in (0..nr).rev() {
            let s = *b;
            for c in 0..4 {
                for row in 0..4 {
                    b[((c + row) % 4) * 4 + row] = s[c * 4 + row];
                }
            }
            for x in b.iter_mut() {
                *x = INV_SBOX[*x as usize];
            }
            for k in 0..16 {
                b[k] ^= self.rk[r][k];
            }
            if r != 0 {
                for c in 0..4 {
                    let a = [b[c * 4], b[c * 4 + 1], b[c * 4 + 2], b[c * 4 + 3]];
                    b[c * 4] = gmul(a[0], 14) ^ gmul(a[1], 11) ^ gmul(a[2], 13) ^ gmul(a[3], 9);
                    b[c * 4 + 1] = gmul(a[0], 9) ^ gmul(a[1], 14) ^ gmul(a[2], 11) ^ gmul(a[3], 13);
                    b[c * 4 + 2] = gmul(a[0], 13) ^ gmul(a[1], 9) ^ gmul(a[2], 14) ^ gmul(a[3], 11);
                    b[c * 4 + 3] = gmul(a[0], 11) ^ gmul(a[1], 13) ^ gmul(a[2], 9) ^ gmul(a[3], 14);
                }
            }
        }
    }
}

/// AES-CBC decryption; the first 16 bytes are the IV. Removes PKCS#7 padding.
pub fn aes_cbc_decrypt(key: &[u8], data: &[u8]) -> Vec<u8> {
    if data.len() < 32 {
        return Vec::new();
    }
    let aes = Aes::new(key);
    let mut prev: [u8; 16] = data[..16].try_into().unwrap();
    let mut out = Vec::with_capacity(data.len());
    for c in data[16..].chunks_exact(16) {
        let mut b: [u8; 16] = c.try_into().unwrap();
        aes.decrypt(&mut b);
        for k in 0..16 {
            b[k] ^= prev[k];
        }
        prev = c.try_into().unwrap();
        out.extend_from_slice(&b);
    }
    if let Some(&pad) = out.last() {
        if (1..=16).contains(&pad) && out.len() >= pad as usize {
            out.truncate(out.len() - pad as usize);
        }
    }
    out
}

/// AES-CBC with no IV prefix and no padding removal (for the R5/R6 key unwrapping).
fn aes_cbc_raw(key: &[u8], iv: &[u8; 16], data: &[u8], encrypt: bool) -> Vec<u8> {
    let aes = Aes::new(key);
    let mut prev = *iv;
    let mut out = Vec::with_capacity(data.len());
    for c in data.chunks_exact(16) {
        let mut b: [u8; 16] = c.try_into().unwrap();
        if encrypt {
            for k in 0..16 {
                b[k] ^= prev[k];
            }
            aes.encrypt(&mut b);
            prev = b;
        } else {
            aes.decrypt(&mut b);
            for k in 0..16 {
                b[k] ^= prev[k];
            }
            prev = c.try_into().unwrap();
        }
        out.extend_from_slice(&b);
    }
    out
}

// ---- SHA-2 ----------------------------------------------------------------------------------

pub fn sha256(data: &[u8]) -> [u8; 32] {
    const K: [u32; 64] = [
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3, 0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
    ];
    let mut h = [0x6a09e667u32, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19];
    let mut msg = data.to_vec();
    let bits = (data.len() as u64).wrapping_mul(8);
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&bits.to_be_bytes());
    for chunk in msg.chunks(64) {
        let mut w = [0u32; 64];
        for i in 0..16 {
            w[i] = u32::from_be_bytes([chunk[i * 4], chunk[i * 4 + 1], chunk[i * 4 + 2], chunk[i * 4 + 3]]);
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16].wrapping_add(s0).wrapping_add(w[i - 7]).wrapping_add(s1);
        }
        let mut v = h;
        for i in 0..64 {
            let s1 = v[4].rotate_right(6) ^ v[4].rotate_right(11) ^ v[4].rotate_right(25);
            let ch = (v[4] & v[5]) ^ (!v[4] & v[6]);
            let t1 = v[7].wrapping_add(s1).wrapping_add(ch).wrapping_add(K[i]).wrapping_add(w[i]);
            let s0 = v[0].rotate_right(2) ^ v[0].rotate_right(13) ^ v[0].rotate_right(22);
            let maj = (v[0] & v[1]) ^ (v[0] & v[2]) ^ (v[1] & v[2]);
            let t2 = s0.wrapping_add(maj);
            v = [t1.wrapping_add(t2), v[0], v[1], v[2], v[3].wrapping_add(t1), v[4], v[5], v[6]];
        }
        for i in 0..8 {
            h[i] = h[i].wrapping_add(v[i]);
        }
    }
    let mut out = [0u8; 32];
    for i in 0..8 {
        out[i * 4..i * 4 + 4].copy_from_slice(&h[i].to_be_bytes());
    }
    out
}

fn sha512_core(data: &[u8], init: [u64; 8], out_len: usize) -> Vec<u8> {
    const K: [u64; 80] = [
        0x428a2f98d728ae22, 0x7137449123ef65cd, 0xb5c0fbcfec4d3b2f, 0xe9b5dba58189dbbc, 0x3956c25bf348b538, 0x59f111f1b605d019, 0x923f82a4af194f9b, 0xab1c5ed5da6d8118, 0xd807aa98a3030242, 0x12835b0145706fbe, 0x243185be4ee4b28c, 0x550c7dc3d5ffb4e2, 0x72be5d74f27b896f, 0x80deb1fe3b1696b1, 0x9bdc06a725c71235, 0xc19bf174cf692694, 0xe49b69c19ef14ad2, 0xefbe4786384f25e3, 0x0fc19dc68b8cd5b5, 0x240ca1cc77ac9c65, 0x2de92c6f592b0275, 0x4a7484aa6ea6e483, 0x5cb0a9dcbd41fbd4, 0x76f988da831153b5, 0x983e5152ee66dfab, 0xa831c66d2db43210, 0xb00327c898fb213f, 0xbf597fc7beef0ee4, 0xc6e00bf33da88fc2, 0xd5a79147930aa725, 0x06ca6351e003826f, 0x142929670a0e6e70, 0x27b70a8546d22ffc, 0x2e1b21385c26c926, 0x4d2c6dfc5ac42aed, 0x53380d139d95b3df, 0x650a73548baf63de, 0x766a0abb3c77b2a8, 0x81c2c92e47edaee6, 0x92722c851482353b, 0xa2bfe8a14cf10364, 0xa81a664bbc423001, 0xc24b8b70d0f89791, 0xc76c51a30654be30, 0xd192e819d6ef5218, 0xd69906245565a910, 0xf40e35855771202a, 0x106aa07032bbd1b8, 0x19a4c116b8d2d0c8, 0x1e376c085141ab53, 0x2748774cdf8eeb99, 0x34b0bcb5e19b48a8, 0x391c0cb3c5c95a63, 0x4ed8aa4ae3418acb, 0x5b9cca4f7763e373, 0x682e6ff3d6b2b8a3, 0x748f82ee5defb2fc, 0x78a5636f43172f60, 0x84c87814a1f0ab72, 0x8cc702081a6439ec, 0x90befffa23631e28, 0xa4506cebde82bde9, 0xbef9a3f7b2c67915, 0xc67178f2e372532b, 0xca273eceea26619c, 0xd186b8c721c0c207, 0xeada7dd6cde0eb1e, 0xf57d4f7fee6ed178, 0x06f067aa72176fba, 0x0a637dc5a2c898a6, 0x113f9804bef90dae, 0x1b710b35131c471b, 0x28db77f523047d84, 0x32caab7b40c72493, 0x3c9ebe0a15c9bebc, 0x431d67c49c100d4c, 0x4cc5d4becb3e42b6, 0x597f299cfc657e2a, 0x5fcb6fab3ad6faec, 0x6c44198c4a475817,
    ];
    let mut h = init;
    let mut msg = data.to_vec();
    let bits = (data.len() as u128).wrapping_mul(8);
    msg.push(0x80);
    while msg.len() % 128 != 112 {
        msg.push(0);
    }
    msg.extend_from_slice(&bits.to_be_bytes());
    for chunk in msg.chunks(128) {
        let mut w = [0u64; 80];
        for i in 0..16 {
            w[i] = u64::from_be_bytes(chunk[i * 8..i * 8 + 8].try_into().unwrap());
        }
        for i in 16..80 {
            let s0 = w[i - 15].rotate_right(1) ^ w[i - 15].rotate_right(8) ^ (w[i - 15] >> 7);
            let s1 = w[i - 2].rotate_right(19) ^ w[i - 2].rotate_right(61) ^ (w[i - 2] >> 6);
            w[i] = w[i - 16].wrapping_add(s0).wrapping_add(w[i - 7]).wrapping_add(s1);
        }
        let mut v = h;
        for i in 0..80 {
            let s1 = v[4].rotate_right(14) ^ v[4].rotate_right(18) ^ v[4].rotate_right(41);
            let ch = (v[4] & v[5]) ^ (!v[4] & v[6]);
            let t1 = v[7].wrapping_add(s1).wrapping_add(ch).wrapping_add(K[i]).wrapping_add(w[i]);
            let s0 = v[0].rotate_right(28) ^ v[0].rotate_right(34) ^ v[0].rotate_right(39);
            let maj = (v[0] & v[1]) ^ (v[0] & v[2]) ^ (v[1] & v[2]);
            let t2 = s0.wrapping_add(maj);
            v = [t1.wrapping_add(t2), v[0], v[1], v[2], v[3].wrapping_add(t1), v[4], v[5], v[6]];
        }
        for i in 0..8 {
            h[i] = h[i].wrapping_add(v[i]);
        }
    }
    let mut out = Vec::with_capacity(64);
    for x in h {
        out.extend_from_slice(&x.to_be_bytes());
    }
    out.truncate(out_len);
    out
}

pub fn sha384(d: &[u8]) -> Vec<u8> {
    sha512_core(d, [0xcbbb9d5dc1059ed8, 0x629a292a367cd507, 0x9159015a3070dd17, 0x152fecd8f70e5939, 0x67332667ffc00b31, 0x8eb44a8768581511, 0xdb0c2e0d64f98fa7, 0x47b5481dbefa4fa4], 48)
}
pub fn sha512(d: &[u8]) -> Vec<u8> {
    sha512_core(d, [0x6a09e667f3bcc908, 0xbb67ae8584caa73b, 0x3c6ef372fe94f82b, 0xa54ff53a5f1d36f1, 0x510e527fade682d1, 0x9b05688c2b3e6c1f, 0x1f83d9abfb41bd6b, 0x5be0cd19137e2179], 64)
}

// ---- the standard security handler -----------------------------------------------------------

const PAD: [u8; 32] = [0x28, 0xBF, 0x4E, 0x5E, 0x4E, 0x75, 0x8A, 0x41, 0x64, 0x00, 0x4E, 0x56, 0xFF, 0xFA, 0x01, 0x08, 0x2E, 0x2E, 0x00, 0xB6, 0xD0, 0x68, 0x3E, 0x80, 0x2F, 0x0C, 0xA9, 0xFE, 0x64, 0x53, 0x69, 0x7A];

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Method {
    Rc4,
    Aes,
    None,
}

#[derive(Clone)]
pub struct Decryptor {
    key: Vec<u8>,
    /// R5/R6 use the file key directly, without per-object keys.
    aes256: bool,
    pub strings: Method,
    pub streams: Method,
}

pub struct Params<'a> {
    pub v: i64,
    pub r: i64,
    pub length: i64,
    pub o: &'a [u8],
    pub u: &'a [u8],
    pub oe: &'a [u8],
    pub ue: &'a [u8],
    pub p: i64,
    pub id0: &'a [u8],
    pub encrypt_metadata: bool,
    pub strings: Method,
    pub streams: Method,
}

/// R6's hash ("2.B" in ISO 32000-2).
fn hash_r6(password: &[u8], salt: &[u8], udata: &[u8]) -> Vec<u8> {
    let mut input = password.to_vec();
    input.extend_from_slice(salt);
    input.extend_from_slice(udata);
    let mut k = sha256(&input).to_vec();
    let mut round = 0u32;
    loop {
        let mut k1 = Vec::new();
        for _ in 0..64 {
            k1.extend_from_slice(password);
            k1.extend_from_slice(&k);
            k1.extend_from_slice(udata);
        }
        let iv: [u8; 16] = k[16..32].try_into().unwrap();
        let e = aes_cbc_raw(&k[..16], &iv, &k1, true);
        let sum: u32 = e[..16].iter().map(|&b| b as u32).sum();
        k = match sum % 3 {
            0 => sha256(&e).to_vec(),
            1 => sha384(&e),
            _ => sha512(&e),
        };
        round += 1;
        if round >= 64 && (*e.last().unwrap() as u32) <= round - 32 {
            break;
        }
        if round > 2000 {
            break;
        }
    }
    k.truncate(32);
    k
}

impl Decryptor {
    /// Work out the file key with the empty user password. None if a password is needed.
    pub fn new(p: &Params) -> Option<Decryptor> {
        if p.r >= 5 {
            // AES-256: validation salt and key salt follow the 32-byte hash in /U.
            if p.u.len() < 48 || p.ue.len() < 32 {
                return None;
            }
            let (hash, vsalt, ksalt) = (&p.u[..32], &p.u[32..40], &p.u[40..48]);
            let check = if p.r == 5 { sha256(vsalt).to_vec() } else { hash_r6(b"", vsalt, b"") };
            if check[..32] != hash[..] {
                return None;
            }
            let ik = if p.r == 5 { sha256(ksalt).to_vec() } else { hash_r6(b"", ksalt, b"") };
            let key = aes_cbc_raw(&ik[..32], &[0u8; 16], &p.ue[..32], false);
            return Some(Decryptor { key, aes256: true, strings: p.strings, streams: p.streams });
        }
        let n = if p.r == 2 { 5 } else { (p.length / 8).clamp(5, 16) as usize };
        let mut input = PAD.to_vec();
        input.extend_from_slice(&p.o[..p.o.len().min(32)]);
        input.extend_from_slice(&(p.p as i32).to_le_bytes());
        input.extend_from_slice(p.id0);
        if p.r >= 4 && !p.encrypt_metadata {
            input.extend_from_slice(&[0xFF; 4]);
        }
        let mut key = md5(&input)[..n].to_vec();
        if p.r >= 3 {
            for _ in 0..50 {
                key = md5(&key)[..n].to_vec();
            }
        }
        // Check against /U.
        let ok = if p.r == 2 {
            rc4(&key, &PAD) == p.u[..p.u.len().min(32)]
        } else {
            let mut h = PAD.to_vec();
            h.extend_from_slice(p.id0);
            let mut x = rc4(&key, &md5(&h));
            for i in 1..=19u8 {
                let k2: Vec<u8> = key.iter().map(|b| b ^ i).collect();
                x = rc4(&k2, &x);
            }
            p.u.len() >= 16 && x[..16] == p.u[..16]
        };
        if !ok {
            return None;
        }
        Some(Decryptor { key, aes256: false, strings: p.strings, streams: p.streams })
    }

    fn object_key(&self, num: u32, gen: u16, aes: bool) -> Vec<u8> {
        if self.aes256 {
            return self.key.clone();
        }
        let mut k = self.key.clone();
        k.extend_from_slice(&num.to_le_bytes()[..3]);
        k.extend_from_slice(&gen.to_le_bytes());
        if aes {
            k.extend_from_slice(b"sAlT");
        }
        let n = (self.key.len() + 5).min(16);
        md5(&k)[..n].to_vec()
    }

    pub fn decrypt(&self, data: &[u8], num: u32, gen: u16, stream: bool) -> Vec<u8> {
        let m = if stream { self.streams } else { self.strings };
        match m {
            Method::None => data.to_vec(),
            Method::Rc4 => rc4(&self.object_key(num, gen, false), data),
            Method::Aes => aes_cbc_decrypt(&self.object_key(num, gen, true), data),
        }
    }
}

#[allow(dead_code)]
pub fn selftest() -> bool {
    // Known answers: MD5(""), SHA-256("abc"), AES-128 FIPS-197 example.
    let md = md5(b"");
    let sh = sha256(b"abc");
    let aes = Aes::new(&[0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a, 0x0b, 0x0c, 0x0d, 0x0e, 0x0f]);
    let mut b = [0x00, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99, 0xaa, 0xbb, 0xcc, 0xdd, 0xee, 0xff];
    aes.encrypt(&mut b);
    let enc_ok = b == [0x69, 0xc4, 0xe0, 0xd8, 0x6a, 0x7b, 0x04, 0x30, 0xd8, 0xcd, 0xb7, 0x80, 0x70, 0xb4, 0xc5, 0x5a];
    aes.decrypt(&mut b);
    md[..4] == [0xd4, 0x1d, 0x8c, 0xd9] && sh[..4] == [0xba, 0x78, 0x16, 0xbf] && enc_ok && b[0] == 0 && b[15] == 0xff && vec![0u8; 0].is_empty()
}
