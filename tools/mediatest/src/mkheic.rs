// HEIC builder for tests (included into main.rs).

/// Build a HEIC like a phone's:
/// mkheic <out> <width> <height> <cols> <rows> <irot 0-3> <thumb.hevc|-> <tile1.hevc> ...
/// (tiles: Annex B HEVC, one intra picture each, same parameter sets).
fn mkheic(a: &[String]) {
    let out = &a[0];
    let (w, h): (u32, u32) = (a[1].parse().unwrap(), a[2].parse().unwrap());
    let (cols, rows): (u8, u8) = (a[3].parse().unwrap(), a[4].parse().unwrap());
    let irot: u8 = a[5].parse().unwrap();
    let thumb = &a[6];
    let tiles = &a[7..];
    fn nals(es: &[u8]) -> Vec<Vec<u8>> {
        let mut starts = Vec::new();
        let mut i = 0;
        while i + 3 <= es.len() {
            if es[i] == 0 && es[i + 1] == 0 && es[i + 2] == 1 {
                starts.push(i + 3);
                i += 3;
            } else {
                i += 1;
            }
        }
        let mut v = Vec::new();
        for (k, &s) in starts.iter().enumerate() {
            let mut e = if k + 1 < starts.len() { starts[k + 1] - 3 } else { es.len() };
            while e > s && es[e - 1] == 0 {
                e -= 1;
            }
            v.push(es[s..e].to_vec());
        }
        v
    }
    let split = |path: &str| -> (Vec<Vec<u8>>, Vec<u8>) {
        let ns = nals(&std::fs::read(path).unwrap());
        let ps: Vec<Vec<u8>> = ns.iter().filter(|n| (32..=34).contains(&((n[0] >> 1) & 63))).cloned().collect();
        let mut data = Vec::new();
        for n in ns.iter().filter(|n| ((n[0] >> 1) & 63) < 32) {
            data.extend_from_slice(&(n.len() as u32).to_be_bytes());
            data.extend_from_slice(n);
        }
        (ps, data)
    };
    let hvcc = |ps: &[Vec<u8>]| -> Vec<u8> {
        let sps = ps.iter().find(|n| ((n[0] >> 1) & 63) == 33).unwrap();
        let mut c = vec![1u8];
        c.extend_from_slice(&sps[3..15]);
        c.truncate(13);
        c.extend_from_slice(&[0xF0, 0x00, 0xFC, 0xFD, 0xF8, 0xF8, 0, 0, 0x0F]);
        c.push(3);
        for t in 32..=34u8 {
            let n = ps.iter().find(|n| ((n[0] >> 1) & 63) == t).unwrap();
            c.push(0x80 | t);
            c.extend_from_slice(&1u16.to_be_bytes());
            c.extend_from_slice(&(n.len() as u16).to_be_bytes());
            c.extend_from_slice(n);
        }
        c
    };
    fn bx(t: &[u8; 4], body: &[u8]) -> Vec<u8> {
        let mut v = ((body.len() + 8) as u32).to_be_bytes().to_vec();
        v.extend_from_slice(t);
        v.extend_from_slice(body);
        v
    }
    fn fbx(t: &[u8; 4], ver: u8, flags: u32, body: &[u8]) -> Vec<u8> {
        let mut b = vec![ver, (flags >> 16) as u8, (flags >> 8) as u8, flags as u8];
        b.extend_from_slice(body);
        bx(t, &b)
    }
    let mut tile_data = Vec::new();
    let mut ps0 = Vec::new();
    for (k, t) in tiles.iter().enumerate() {
        let (ps, d) = split(t);
        if k == 0 {
            ps0 = ps;
        }
        tile_data.push(d);
    }
    let tile_w = w.div_ceil(cols as u32);
    let tile_h = h.div_ceil(rows as u32);
    let has_thumb = thumb != "-";
    let (tps, tdata) = if has_thumb { split(thumb) } else { (Vec::new(), Vec::new()) };
    let n_tiles = tiles.len() as u16;
    let thumb_id = 2 + n_tiles;
    let count = 1 + n_tiles + has_thumb as u16;
    let mut iinf_body = count.to_be_bytes().to_vec();
    let infe = |id: u16, t: &[u8; 4], hidden: bool| -> Vec<u8> {
        let mut b = id.to_be_bytes().to_vec();
        b.extend_from_slice(&0u16.to_be_bytes());
        b.extend_from_slice(t);
        b.push(0);
        fbx(b"infe", 2, hidden as u32, &b)
    };
    iinf_body.extend(infe(1, b"grid", false));
    for k in 0..n_tiles {
        iinf_body.extend(infe(2 + k, b"hvc1", true));
    }
    if has_thumb {
        iinf_body.extend(infe(thumb_id, b"hvc1", false));
    }
    let mut dimg = 1u16.to_be_bytes().to_vec();
    dimg.extend_from_slice(&n_tiles.to_be_bytes());
    for k in 0..n_tiles {
        dimg.extend_from_slice(&(2 + k).to_be_bytes());
    }
    let mut iref_body = bx(b"dimg", &dimg);
    if has_thumb {
        let mut th = thumb_id.to_be_bytes().to_vec();
        th.extend_from_slice(&1u16.to_be_bytes());
        th.extend_from_slice(&1u16.to_be_bytes());
        iref_body.extend(bx(b"thmb", &th));
    }
    let ispe = |w: u32, h: u32| {
        let mut b = w.to_be_bytes().to_vec();
        b.extend_from_slice(&h.to_be_bytes());
        fbx(b"ispe", 0, 0, &b)
    };
    // Properties: 1 hvcC, 2 ispe tile, 3 ispe grid, 4 irot, 5 thumb hvcC, 6 thumb ispe.
    let mut ipco = Vec::new();
    ipco.extend(bx(b"hvcC", &hvcc(&ps0)));
    ipco.extend(ispe(tile_w, tile_h));
    ipco.extend(ispe(w, h));
    ipco.extend(bx(b"irot", &[irot]));
    if has_thumb {
        ipco.extend(bx(b"hvcC", &hvcc(&tps)));
        ipco.extend(ispe(320, 240));
    }
    let mut ipma = (count as u32).to_be_bytes().to_vec();
    ipma.extend_from_slice(&1u16.to_be_bytes());
    ipma.extend_from_slice(&[2, 3, 4]);
    for k in 0..n_tiles {
        ipma.extend_from_slice(&(2 + k).to_be_bytes());
        ipma.extend_from_slice(&[2, 0x81, 2]);
    }
    if has_thumb {
        ipma.extend_from_slice(&thumb_id.to_be_bytes());
        ipma.extend_from_slice(&[3, 0x85, 6, 4]);
    }
    let iprp = bx(b"iprp", &[bx(b"ipco", &ipco), fbx(b"ipma", 0, 0, &ipma)].concat());
    let mut grid = vec![0u8, 0, rows - 1, cols - 1];
    grid.extend_from_slice(&(w as u16).to_be_bytes());
    grid.extend_from_slice(&(h as u16).to_be_bytes());
    let idat = bx(b"idat", &grid);
    let mut hdlr = vec![0u8; 4];
    hdlr.extend_from_slice(b"pict");
    hdlr.extend_from_slice(&[0; 13]);
    let ftyp = bx(b"ftyp", b"heic\0\0\0\0mif1heic");
    let build_iloc = |mdat_at: u32| -> Vec<u8> {
        let mut b = vec![0x44, 0x00];
        b.extend_from_slice(&count.to_be_bytes());
        b.extend_from_slice(&1u16.to_be_bytes());
        b.extend_from_slice(&1u16.to_be_bytes());
        b.extend_from_slice(&0u16.to_be_bytes());
        b.extend_from_slice(&1u16.to_be_bytes());
        b.extend_from_slice(&0u32.to_be_bytes());
        b.extend_from_slice(&(grid.len() as u32).to_be_bytes());
        let mut off = mdat_at;
        let all: Vec<&Vec<u8>> = tile_data.iter().chain(if has_thumb { Some(&tdata) } else { None }).collect();
        for (k, d) in all.iter().enumerate() {
            let id = if k < n_tiles as usize { 2 + k as u16 } else { thumb_id };
            b.extend_from_slice(&id.to_be_bytes());
            b.extend_from_slice(&0u16.to_be_bytes());
            b.extend_from_slice(&0u16.to_be_bytes());
            b.extend_from_slice(&1u16.to_be_bytes());
            b.extend_from_slice(&off.to_be_bytes());
            b.extend_from_slice(&(d.len() as u32).to_be_bytes());
            off += d.len() as u32;
        }
        fbx(b"iloc", 1, 0, &b)
    };
    let meta_of = |iloc: Vec<u8>| fbx(b"meta", 0, 0, &[fbx(b"hdlr", 0, 0, &hdlr), fbx(b"pitm", 0, 0, &1u16.to_be_bytes()), fbx(b"iinf", 0, 0, &iinf_body), fbx(b"iref", 0, 0, &iref_body), iprp.clone(), idat.clone(), iloc].concat());
    let meta_len = meta_of(build_iloc(0)).len();
    let mdat_at = (ftyp.len() + meta_len + 8) as u32;
    let meta = meta_of(build_iloc(mdat_at));
    let mut body = Vec::new();
    for d in &tile_data {
        body.extend_from_slice(d);
    }
    if has_thumb {
        body.extend_from_slice(&tdata);
    }
    let file = [ftyp, meta, bx(b"mdat", &body)].concat();
    std::fs::write(out, file).unwrap();
    println!("wrote {} ({} tiles {}x{})", out, n_tiles, tile_w, tile_h);
}
