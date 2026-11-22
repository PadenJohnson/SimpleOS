//! HEIC / HEIF (iPhone photos and some Android phones): the ISO box structure, single or
//! grid (tiled) HEVC pictures, rotation and mirroring, and the small thumbnail for quick
//! previews. AVIF (AV1 pictures) isn't supported.

use super::orient;
use crate::image::Image;
use crate::media::dec::{Pic, VideoDec};
use alloc::vec;
use alloc::vec::Vec;

fn be(d: &[u8], o: usize, n: usize) -> Option<u64> {
    let s = d.get(o..o + n)?;
    Some(s.iter().fold(0u64, |a, &b| a << 8 | b as u64))
}

/// Child boxes in d[start..end]: (type, payload start, payload end).
fn boxes(d: &[u8], start: usize, end: usize) -> Vec<([u8; 4], usize, usize)> {
    let mut out = Vec::new();
    let mut p = start;
    let end = end.min(d.len());
    while p + 8 <= end {
        let size = be(d, p, 4).unwrap_or(0) as usize;
        let t = [d[p + 4], d[p + 5], d[p + 6], d[p + 7]];
        let (hdr, size) = match size {
            1 => (16, be(d, p + 8, 8).unwrap_or(0) as usize),
            0 => (8, end - p),
            s => (8, s),
        };
        if size < hdr || p + size > end {
            break;
        }
        out.push((t, p + hdr, p + size));
        p += size;
    }
    out
}

struct Item {
    id: u32,
    kind: [u8; 4],
    /// (construction method, extents (offset, length)).
    method: u16,
    extents: Vec<(u64, u64)>,
    props: Vec<usize>,
}

struct Heif<'a> {
    d: &'a [u8],
    primary: u32,
    items: Vec<Item>,
    /// (type, from, to list)
    refs: Vec<([u8; 4], u32, Vec<u32>)>,
    /// Property boxes (type, payload start, payload end), 1-based in ipma.
    props: Vec<([u8; 4], usize, usize)>,
    idat: Option<(usize, usize)>,
}

impl<'a> Heif<'a> {
    fn parse(d: &'a [u8]) -> Result<Heif<'a>, &'static str> {
        let err = "Damaged HEIC file";
        let top = boxes(d, 0, d.len());
        let ftyp = top.iter().find(|b| &b.0 == b"ftyp").ok_or("Not a valid HEIC")?;
        let brands = &d[ftyp.1..ftyp.2];
        let has = |b: &[u8; 4]| brands.chunks(4).any(|c| c == b);
        if (has(b"avif") || has(b"avis")) && !has(b"heic") && !has(b"heix") {
            return Err("AVIF photos can't be opened yet");
        }
        let meta = top.iter().find(|b| &b.0 == b"meta").ok_or(err)?;
        let mut h = Heif { d, primary: 0, items: Vec::new(), refs: Vec::new(), props: Vec::new(), idat: None };
        for (t, s, e) in boxes(d, meta.1 + 4, meta.2) {
            let v = d.get(s).copied().unwrap_or(0);
            match &t {
                b"pitm" => h.primary = if v == 0 { be(d, s + 4, 2) } else { be(d, s + 4, 4) }.ok_or(err)? as u32,
                b"iinf" => {
                    let first = if v == 0 { s + 6 } else { s + 8 };
                    for (t2, s2, _) in boxes(d, first, e) {
                        if &t2 != b"infe" {
                            continue;
                        }
                        let iv = d.get(s2).copied().unwrap_or(0);
                        if iv < 2 {
                            continue;
                        }
                        let (id, at) = if iv == 2 { (be(d, s2 + 4, 2), s2 + 8) } else { (be(d, s2 + 4, 4), s2 + 10) };
                        let id = id.ok_or(err)? as u32;
                        let kind = d.get(at..at + 4).ok_or(err)?;
                        let kind = [kind[0], kind[1], kind[2], kind[3]];
                        match h.items.iter_mut().find(|i| i.id == id) {
                            Some(i) => i.kind = kind,
                            None => h.items.push(Item { id, kind, method: 0, extents: Vec::new(), props: Vec::new() }),
                        }
                    }
                }
                b"iloc" => {
                    let a = d.get(s + 4).copied().unwrap_or(0);
                    let b = d.get(s + 5).copied().unwrap_or(0);
                    let (os, ls, bs, is) = ((a >> 4) as usize, (a & 15) as usize, (b >> 4) as usize, if v >= 1 { (b & 15) as usize } else { 0 });
                    let (count, mut p) = if v < 2 { (be(d, s + 6, 2).ok_or(err)?, s + 8) } else { (be(d, s + 6, 4).ok_or(err)?, s + 10) };
                    for _ in 0..count.min(10_000) {
                        let id = if v < 2 { be(d, p, 2) } else { be(d, p, 4) }.ok_or(err)? as u32;
                        p += if v < 2 { 2 } else { 4 };
                        let mut method = 0;
                        if v >= 1 {
                            method = (be(d, p, 2).ok_or(err)? & 15) as u16;
                            p += 2;
                        }
                        p += 2; // data reference index
                        let base = if bs > 0 { be(d, p, bs).ok_or(err)? } else { 0 };
                        p += bs;
                        let n = be(d, p, 2).ok_or(err)? as usize;
                        p += 2;
                        let mut extents = Vec::new();
                        for _ in 0..n {
                            p += is;
                            let off = if os > 0 { be(d, p, os).ok_or(err)? } else { 0 };
                            p += os;
                            let len = if ls > 0 { be(d, p, ls).ok_or(err)? } else { 0 };
                            p += ls;
                            extents.push((base + off, len));
                        }
                        match h.items.iter_mut().find(|i| i.id == id) {
                            Some(i) => {
                                i.method = method;
                                i.extents = extents;
                            }
                            None => h.items.push(Item { id, kind: *b"????", method, extents, props: Vec::new() }),
                        }
                    }
                }
                b"iref" => {
                    for (t2, s2, e2) in boxes(d, s + 4, e) {
                        let w = if v == 0 { 2 } else { 4 };
                        let from = be(d, s2, w).ok_or(err)? as u32;
                        let n = be(d, s2 + w, 2).ok_or(err)? as usize;
                        let mut to = Vec::new();
                        for k in 0..n {
                            if s2 + w + 2 + (k + 1) * w > e2 {
                                break;
                            }
                            to.push(be(d, s2 + w + 2 + k * w, w).ok_or(err)? as u32);
                        }
                        h.refs.push((t2, from, to));
                    }
                }
                b"iprp" => {
                    for (t2, s2, e2) in boxes(d, s, e) {
                        if &t2 == b"ipco" {
                            h.props = boxes(d, s2, e2);
                        } else if &t2 == b"ipma" {
                            let pv = d.get(s2).copied().unwrap_or(0);
                            let flags = d.get(s2 + 3).copied().unwrap_or(0);
                            let n = be(d, s2 + 4, 4).ok_or(err)? as usize;
                            let mut p = s2 + 8;
                            for _ in 0..n.min(10_000) {
                                let id = if pv < 1 { be(d, p, 2) } else { be(d, p, 4) }.ok_or(err)? as u32;
                                p += if pv < 1 { 2 } else { 4 };
                                let cnt = d.get(p).copied().ok_or(err)? as usize;
                                p += 1;
                                let mut list = Vec::new();
                                for _ in 0..cnt {
                                    let idx = if flags & 1 != 0 {
                                        let v = be(d, p, 2).ok_or(err)? as usize;
                                        p += 2;
                                        v & 0x7FFF
                                    } else {
                                        let v = d.get(p).copied().ok_or(err)? as usize;
                                        p += 1;
                                        v & 0x7F
                                    };
                                    list.push(idx);
                                }
                                if let Some(i) = h.items.iter_mut().find(|i| i.id == id) {
                                    i.props = list;
                                } else {
                                    h.items.push(Item { id, kind: *b"????", method: 0, extents: Vec::new(), props: list });
                                }
                            }
                        }
                    }
                }
                b"idat" => h.idat = Some((s, e)),
                _ => {}
            }
        }
        Ok(h)
    }

    fn item(&self, id: u32) -> Option<&Item> {
        self.items.iter().find(|i| i.id == id)
    }

    fn data(&self, it: &Item) -> Option<Vec<u8>> {
        let mut v = Vec::new();
        for &(off, len) in &it.extents {
            let (base, limit) = if it.method == 1 { self.idat? } else { (0, self.d.len()) };
            let s = base + off as usize;
            let e = if len == 0 { limit } else { s + len as usize };
            v.extend_from_slice(self.d.get(s..e.min(limit))?);
        }
        Some(v)
    }

    fn prop(&self, it: &Item, kind: &[u8; 4]) -> Option<(usize, usize)> {
        it.props.iter().filter_map(|&i| self.props.get(i.wrapping_sub(1))).find(|p| &p.0 == kind).map(|p| (p.1, p.2))
    }

    fn refs_from(&self, kind: &[u8; 4], from: u32) -> Vec<u32> {
        self.refs.iter().filter(|r| &r.0 == kind && r.1 == from).flat_map(|r| r.2.iter().copied()).collect()
    }

    /// Colour settings (BT.709?, full range?) from an nclx colr property.
    fn colour(&self, it: &Item) -> Option<(bool, bool)> {
        let (s, e) = self.prop(it, b"colr")?;
        if e - s >= 11 && &self.d[s..s + 4] == b"nclx" {
            let matrix = be(self.d, s + 8, 2)?;
            let full = self.d[s + 10] & 0x80 != 0;
            return Some((matrix == 1 || matrix == 9, full));
        }
        None
    }

    /// One HEVC-coded item to pixels.
    fn hvc1(&self, it: &Item, colour: Option<(bool, bool)>) -> Result<Image, &'static str> {
        let (s, e) = self.prop(it, b"hvcC").ok_or("Damaged HEIC file")?;
        let data = self.data(it).ok_or("Damaged HEIC file")?;
        let mut dec = crate::media::hevc::HevcDec::new(&self.d[s..e]);
        dec.decode(&data);
        dec.flush();
        let Some(Pic::Yuv(mut f)) = dec.pop() else { return Err("This HEIC picture couldn't be decoded") };
        if let Some((bt709, full)) = colour {
            f.bt709 = bt709;
            f.full_range = full;
        }
        let px = f.to_rgb(f.width, f.height).into_iter().map(|p| p | 0xFF00_0000).collect();
        super::image(f.width, f.height, px)
    }

    /// The picture of item `id` (grid or single), before rotation.
    fn picture(&self, id: u32) -> Result<Image, &'static str> {
        let err = "Damaged HEIC file";
        let it = self.item(id).ok_or(err)?;
        let colour = self.colour(it);
        match &it.kind {
            b"hvc1" => self.hvc1(it, colour),
            b"grid" => {
                let g = self.data(it).ok_or(err)?;
                let flags = *g.get(1).ok_or(err)?;
                let rows = *g.get(2).ok_or(err)? as usize + 1;
                let cols = *g.get(3).ok_or(err)? as usize + 1;
                let (ow, oh) = if flags & 1 != 0 { (be(&g, 4, 4).ok_or(err)? as usize, be(&g, 8, 4).ok_or(err)? as usize) } else { (be(&g, 4, 2).ok_or(err)? as usize, be(&g, 6, 2).ok_or(err)? as usize) };
                crate::image::check_size(ow, oh)?;
                let tiles = self.refs_from(b"dimg", id);
                if tiles.len() < rows * cols {
                    return Err(err);
                }
                let mut px = vec![0xFF00_0000u32; ow * oh];
                let (mut tw, mut th) = (0, 0);
                for (k, &t) in tiles.iter().take(rows * cols).enumerate() {
                    let ti = self.item(t).ok_or(err)?;
                    let col = colour.or_else(|| self.colour(ti));
                    let img = self.hvc1(ti, col)?;
                    if k == 0 {
                        tw = img.w;
                        th = img.h;
                    }
                    let (x0, y0) = ((k % cols) * tw, (k / cols) * th);
                    for y in 0..img.h {
                        if y0 + y >= oh {
                            break;
                        }
                        for x in 0..img.w {
                            if x0 + x < ow {
                                px[(y0 + y) * ow + x0 + x] = img.px[y * img.w + x];
                            }
                        }
                    }
                }
                super::image(ow, oh, px)
            }
            b"av01" => Err("AVIF photos can't be opened yet"),
            _ => Err("This HEIC file's picture type isn't supported"),
        }
    }

    /// Apply the item's rotation / mirroring properties in order.
    fn transform(&self, id: u32, mut img: Image) -> Image {
        let Some(it) = self.item(id) else { return img };
        for &pi in &it.props {
            let Some(&(t, s, _)) = self.props.get(pi.wrapping_sub(1)) else { continue };
            let v = self.d.get(s).copied().unwrap_or(0);
            img = match &t {
                b"irot" => orient(img, [1, 8, 3, 6][(v & 3) as usize]),
                b"imir" => orient(img, if v & 1 == 0 { 2 } else { 4 }),
                _ => img,
            };
        }
        img
    }

    /// Full size (after rotation) of the primary picture.
    fn size(&self) -> Option<(usize, usize)> {
        let it = self.item(self.primary)?;
        let (s, _) = self.prop(it, b"ispe")?;
        let (w, h) = (be(self.d, s + 4, 4)? as usize, be(self.d, s + 8, 4)? as usize);
        let turned = it.props.iter().filter_map(|&i| self.props.get(i.wrapping_sub(1))).any(|p| &p.0 == b"irot" && self.d.get(p.1).is_some_and(|v| v & 1 == 1));
        Some(if turned { (h, w) } else { (w, h) })
    }
}

pub fn decode(d: &[u8]) -> Result<Image, &'static str> {
    let h = Heif::parse(d)?;
    let img = h.picture(h.primary)?;
    Ok(h.transform(h.primary, img))
}

/// A quick preview: the embedded thumbnail when it's at least `min` pixels on its longer side.
/// Returns (picture, full size).
pub fn decode_thumb(d: &[u8], min: usize) -> Option<(Image, (usize, usize))> {
    let h = Heif::parse(d).ok()?;
    let size = h.size()?;
    let thumb = h.refs.iter().find(|r| &r.0 == b"thmb" && r.2.contains(&h.primary)).map(|r| r.1)?;
    let img = h.picture(thumb).ok()?;
    if img.w.max(img.h) < min {
        return None;
    }
    Some((h.transform(thumb, img), size))
}

pub fn is_heif(d: &[u8]) -> bool {
    d.len() > 12 && &d[4..8] == b"ftyp" && {
        let n = (be(d, 0, 4).unwrap_or(0) as usize).min(d.len());
        d[8..n].chunks(4).any(|c| matches!(c, b"heic" | b"heix" | b"mif1" | b"msf1" | b"heim" | b"heis" | b"hevc" | b"hevx" | b"avif" | b"avis"))
    }
}
