// PSD and ICO builders for tests (included into main.rs). Input: a PNG/JPEG decoded by Solon.

/// mkpsd <in> <out.psd>: an RGBA Photoshop file with RLE (PackBits) image data.
fn mkpsd(input: &str, out: &str) {
    let img = image::decode(&std::fs::read(input).unwrap()).unwrap();
    let (w, h) = (img.w, img.h);
    let mut f = b"8BPS".to_vec();
    f.extend_from_slice(&1u16.to_be_bytes());
    f.extend_from_slice(&[0; 6]);
    f.extend_from_slice(&4u16.to_be_bytes());
    f.extend_from_slice(&(h as u32).to_be_bytes());
    f.extend_from_slice(&(w as u32).to_be_bytes());
    f.extend_from_slice(&8u16.to_be_bytes());
    f.extend_from_slice(&3u16.to_be_bytes());
    f.extend_from_slice(&0u32.to_be_bytes()); // colour mode data
    f.extend_from_slice(&0u32.to_be_bytes()); // resources
    f.extend_from_slice(&0u32.to_be_bytes()); // layers
    f.extend_from_slice(&1u16.to_be_bytes()); // RLE
    let mut rows: Vec<Vec<u8>> = Vec::new();
    for c in 0..4 {
        let shift = [16, 8, 0, 24][c];
        for y in 0..h {
            let line: Vec<u8> = (0..w).map(|x| (img.px[y * w + x] >> shift) as u8).collect();
            // PackBits: runs of 3+ equal bytes, literals otherwise.
            let mut enc = Vec::new();
            let mut i = 0;
            while i < line.len() {
                let mut run = 1;
                while i + run < line.len() && run < 128 && line[i + run] == line[i] {
                    run += 1;
                }
                if run >= 3 {
                    enc.push((1i32 - run as i32) as i8 as u8);
                    enc.push(line[i]);
                    i += run;
                } else {
                    let start = i;
                    while i < line.len() && i - start < 128 && !(i + 2 < line.len() && line[i] == line[i + 1] && line[i] == line[i + 2]) {
                        i += 1;
                    }
                    enc.push((i - start - 1) as u8);
                    enc.extend_from_slice(&line[start..i]);
                }
            }
            rows.push(enc);
        }
    }
    for r in &rows {
        f.extend_from_slice(&(r.len() as u16).to_be_bytes());
    }
    for r in &rows {
        f.extend_from_slice(r);
    }
    std::fs::write(out, f).unwrap();
}

/// mkico <in> <out.ico> <png|bmp>: a one-picture icon (picture must be at most 256x256).
fn mkico(input: &str, out: &str, kind: &str) {
    let src = std::fs::read(input).unwrap();
    let img = image::decode(&src).unwrap();
    let (w, h) = (img.w, img.h);
    let data = if kind == "png" {
        image::encode_png(&img)
    } else {
        let mut d = Vec::new();
        d.extend_from_slice(&40u32.to_le_bytes());
        d.extend_from_slice(&(w as i32).to_le_bytes());
        d.extend_from_slice(&((h * 2) as i32).to_le_bytes());
        d.extend_from_slice(&1u16.to_le_bytes());
        d.extend_from_slice(&32u16.to_le_bytes());
        d.extend_from_slice(&[0; 24]);
        for y in (0..h).rev() {
            for x in 0..w {
                let p = img.px[y * w + x];
                d.extend_from_slice(&[p as u8, (p >> 8) as u8, (p >> 16) as u8, (p >> 24) as u8]);
            }
        }
        d.extend(std::iter::repeat_n(0u8, w.div_ceil(32) * 4 * h));
        d
    };
    let mut f = vec![0u8, 0, 1, 0, 1, 0];
    f.extend_from_slice(&[(w % 256) as u8, (h % 256) as u8, 0, 0, 1, 0, 32, 0]);
    f.extend_from_slice(&(data.len() as u32).to_le_bytes());
    f.extend_from_slice(&22u32.to_le_bytes());
    f.extend_from_slice(&data);
    std::fs::write(out, f).unwrap();
}
