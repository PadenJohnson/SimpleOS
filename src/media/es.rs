//! Looking inside picture data without decoding it: is this a key frame, and how big is
//! the picture (for streams whose container doesn't say).

use super::VCodec;

/// Start codes (00 00 01) in `d`: the index of the byte after each.
fn start_codes(d: &[u8]) -> impl Iterator<Item = usize> + '_ {
    let mut i = 0;
    core::iter::from_fn(move || {
        while i + 3 <= d.len() {
            if d[i] == 0 && d[i + 1] == 0 && d[i + 2] == 1 {
                i += 3;
                return Some(i);
            }
            i += 1;
        }
        None
    })
}

/// Does this frame (or stream piece) start a picture that can be decoded on its own?
pub fn is_key(codec: &VCodec, d: &[u8]) -> bool {
    match codec {
        VCodec::H264AnnexB => start_codes(d).any(|i| d.get(i).is_some_and(|&b| matches!(b & 0x1F, 5 | 7))),
        VCodec::HevcAnnexB => start_codes(d).any(|i| d.get(i).is_some_and(|&b| matches!((b >> 1) & 0x3F, 16..=21 | 32..=34))),
        VCodec::Mpeg2 { .. } => start_codes(d).any(|i| match d.get(i) {
            Some(0xB3) | Some(0xB8) => true,
            // Picture header: 10 bits of temporal reference, then the type (1 = I).
            Some(0x00) => d.get(i + 2).is_some_and(|&b| (b >> 3) & 7 == 1),
            _ => false,
        }),
        VCodec::Mpeg4 { .. } => start_codes(d).any(|i| match d.get(i) {
            Some(0xB0) | Some(0xB3) => true,
            Some(&b) if (0x20..=0x2F).contains(&b) => true, // VOL header
            Some(0xB6) => d.get(i + 1).is_some_and(|&b| b >> 6 == 0), // I-VOP
            _ => false,
        }),
        _ => true,
    }
}

/// The picture size from an MPEG-1/2 sequence header, if there is one.
pub fn mpeg2_size(d: &[u8]) -> Option<(u32, u32)> {
    let i = start_codes(d).find(|&i| d.get(i) == Some(&0xB3))?;
    let b = d.get(i + 1..i + 4)?;
    Some((((b[0] as u32) << 4) | (b[1] as u32 >> 4), (((b[1] & 15) as u32) << 8) | b[2] as u32))
}

/// What kind of picture data a program stream holds, from its first bytes.
pub fn guess_video(d: &[u8]) -> VCodec {
    for i in start_codes(d) {
        match d.get(i) {
            Some(0xB3) => return VCodec::Mpeg2 { config: alloc::vec::Vec::new() },
            Some(0xB0) | Some(0xB5) => return VCodec::Mpeg4 { config: alloc::vec::Vec::new() },
            Some(&b) if b & 0x1F == 7 && b & 0x80 == 0 => return VCodec::H264AnnexB,
            _ => {}
        }
    }
    VCodec::Mpeg2 { config: alloc::vec::Vec::new() }
}
