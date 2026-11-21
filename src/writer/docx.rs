//! Word documents (.docx): reading the text with its formatting (styles, headings, lists,
//! alignment, page size), and writing Writer documents back as .docx files Word opens.
//! Not kept (yet): pictures, text boxes, and table layout (table text comes in as paragraphs).

use alloc::collections::BTreeMap;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use super::doc::{Align, Block, Doc, DocImage, List, PStyle, Para, Style, PAGE_BREAK, PICTURE};
use crate::doc::xml::{attr, escape, local, Ev, Reader};
use crate::doc::zip;
use crate::gfx::rgb;

#[derive(Clone, Copy, Default)]
struct RP {
    serif: Option<bool>,
    bold: Option<bool>,
    italic: Option<bool>,
    underline: Option<bool>,
    strike: Option<bool>,
    size: Option<u16>,
    color: Option<u32>,
    highlight: Option<Option<u32>>,
}

impl RP {
    fn over(&mut self, o: &RP) {
        macro_rules! take {
            ($($f:ident),*) => { $( if o.$f.is_some() { self.$f = o.$f; } )* };
        }
        take!(serif, bold, italic, underline, strike, size, color, highlight);
    }
    fn apply(&self, s: &mut Style) {
        if let Some(v) = self.serif {
            s.serif = v;
        }
        if let Some(v) = self.bold {
            s.bold = v;
        }
        if let Some(v) = self.italic {
            s.italic = v;
        }
        if let Some(v) = self.underline {
            s.underline = v;
        }
        if let Some(v) = self.strike {
            s.strike = v;
        }
        if let Some(v) = self.size {
            s.size = v.clamp(2, 400);
        }
        if let Some(v) = self.color {
            s.color = v;
        }
        if let Some(v) = self.highlight {
            s.highlight = v;
        }
    }
}

#[derive(Clone, Copy, Default)]
struct PP {
    align: Option<Align>,
    after: Option<u16>,
    line: Option<u16>,
    indent: Option<u8>,
    num: Option<(u32, u8)>,
}

impl PP {
    fn over(&mut self, o: &PP) {
        if o.align.is_some() {
            self.align = o.align;
        }
        if o.after.is_some() {
            self.after = o.after;
        }
        if o.line.is_some() {
            self.line = o.line;
        }
        if o.indent.is_some() {
            self.indent = o.indent;
        }
        if o.num.is_some() {
            self.num = o.num;
        }
    }
}

#[derive(Clone, Default)]
struct SDef {
    based: Option<String>,
    rp: RP,
    pp: PP,
    block: Option<Block>,
}

fn on(attrs: &[(&str, String)]) -> bool {
    !matches!(attr(attrs, "val"), Some("0") | Some("false") | Some("off") | Some("none"))
}

fn hex(s: &str) -> Option<u32> {
    if s.len() == 6 {
        u32::from_str_radix(s, 16).ok()
    } else {
        None
    }
}

fn is_serif(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    !n.contains("sans") && ["times", "georgia", "cambria", "garamond", "serif", "antiqua", "palatino", "century", "baskerville", "minion", "constantia", "bookman", "courier"].iter().any(|k| n.contains(k))
}

fn highlight_named(s: &str) -> Option<u32> {
    Some(match s {
        "yellow" => rgb(255, 240, 0),
        "green" => rgb(140, 240, 120),
        "cyan" => rgb(120, 220, 255),
        "magenta" => rgb(255, 160, 220),
        "blue" => rgb(120, 160, 255),
        "red" => rgb(255, 120, 120),
        "darkYellow" => rgb(200, 180, 60),
        "lightGray" => rgb(210, 210, 210),
        "darkGray" => rgb(150, 150, 150),
        "darkBlue" | "darkCyan" | "darkGreen" | "darkMagenta" | "darkRed" | "black" => rgb(170, 170, 200),
        _ => return None,
    })
}

fn block_of(id: &str, name: &str) -> Option<Block> {
    let k = format!("{} {}", id, name).to_ascii_lowercase().replace(' ', "");
    if k.contains("subtitle") {
        return None;
    }
    if k.starts_with("title") {
        return Some(Block::Title);
    }
    for (n, b) in [("1", Block::H1), ("2", Block::H2), ("3", Block::H3)] {
        if k.contains(&format!("heading{}", n)) {
            return Some(b);
        }
    }
    None
}

/// Run and paragraph properties from a <w:rPr> / <w:pPr> child element.
fn rp_child(rp: &mut RP, name: &str, attrs: &[(&str, String)]) {
    match name {
        "b" => rp.bold = Some(on(attrs)),
        "i" => rp.italic = Some(on(attrs)),
        "u" => rp.underline = Some(on(attrs) && attr(attrs, "val") != Some("none")),
        "strike" | "dstrike" => rp.strike = Some(on(attrs)),
        "sz" => rp.size = attr(attrs, "val").and_then(|v| v.parse().ok()),
        "color" => rp.color = attr(attrs, "val").and_then(|v| if v == "auto" { Some(0) } else { hex(v) }),
        "highlight" => rp.highlight = Some(attr(attrs, "val").and_then(highlight_named)),
        "shd" => {
            if let Some(f) = attr(attrs, "fill").and_then(hex) {
                if f != 0xFFFFFF {
                    rp.highlight = Some(Some(f));
                }
            }
        }
        "rFonts" => {
            if let Some(n) = attr(attrs, "ascii").or_else(|| attr(attrs, "hAnsi")) {
                rp.serif = Some(is_serif(n));
            } else if attr(attrs, "asciiTheme").is_some() {
                rp.serif = Some(false);
            }
        }
        _ => {}
    }
}

fn pp_child(pp: &mut PP, name: &str, attrs: &[(&str, String)]) {
    match name {
        "jc" => {
            pp.align = Some(match attr(attrs, "val").unwrap_or("left") {
                "center" => Align::Center,
                "right" | "end" => Align::Right,
                "both" | "distribute" => Align::Justify,
                _ => Align::Left,
            })
        }
        "spacing" => {
            if let Some(a) = attr(attrs, "after").and_then(|v| v.parse::<u32>().ok()) {
                pp.after = Some((a / 20).min(200) as u16);
            }
            if let Some(l) = attr(attrs, "line").and_then(|v| v.parse::<u32>().ok()) {
                if attr(attrs, "lineRule").is_none_or(|r| r == "auto") {
                    pp.line = Some((l * 100 / 240).clamp(80, 300) as u16);
                }
            }
        }
        "ind" => {
            if let Some(l) = attr(attrs, "left").or_else(|| attr(attrs, "start")).and_then(|v| v.parse::<i32>().ok()) {
                pp.indent = Some((l.max(0) / 720).min(8) as u8);
            }
        }
        _ => {}
    }
}

fn read_styles(b: &[u8]) -> (RP, PP, BTreeMap<String, SDef>, Option<String>) {
    let mut defaults = RP::default();
    let mut pdefault = PP::default();
    let mut map = BTreeMap::new();
    let mut default_para = None;
    let mut r = Reader::new(b);
    let mut cur: Option<(String, SDef, String)> = None;
    let mut in_rpr = false;
    let mut in_ppr = false;
    let mut in_defaults = false;
    while let Some(ev) = r.next() {
        match ev {
            Ev::Start { name, attrs, empty } => {
                let n = local(name);
                match n {
                    "docDefaults" => in_defaults = true,
                    "style" => {
                        let id = String::from(attr(&attrs, "styleId").unwrap_or(""));
                        if attr(&attrs, "type") == Some("paragraph") && attr(&attrs, "default") == Some("1") {
                            default_para = Some(id.clone());
                        }
                        cur = Some((id, SDef::default(), String::new()));
                    }
                    "name" => {
                        if let Some((_, _, nm)) = &mut cur {
                            *nm = String::from(attr(&attrs, "val").unwrap_or(""));
                        }
                    }
                    "basedOn" => {
                        if let Some((_, d, _)) = &mut cur {
                            d.based = attr(&attrs, "val").map(String::from);
                        }
                    }
                    "rPr" if !empty => in_rpr = true,
                    "pPr" if !empty => in_ppr = true,
                    _ => {
                        if in_rpr {
                            match (&mut cur, in_defaults) {
                                (Some((_, d, _)), _) => rp_child(&mut d.rp, n, &attrs),
                                (None, true) => rp_child(&mut defaults, n, &attrs),
                                _ => {}
                            }
                        } else if in_ppr {
                            match (&mut cur, in_defaults) {
                                (Some((_, d, _)), _) => pp_child(&mut d.pp, n, &attrs),
                                (None, true) => pp_child(&mut pdefault, n, &attrs),
                                _ => {}
                            }
                        }
                    }
                }
            }
            Ev::End(name) => match local(name) {
                "docDefaults" => in_defaults = false,
                "rPr" => in_rpr = false,
                "pPr" => in_ppr = false,
                "style" => {
                    if let Some((id, mut d, nm)) = cur.take() {
                        d.block = block_of(&id, &nm);
                        map.insert(id, d);
                    }
                }
                _ => {}
            },
            _ => {}
        }
    }
    (defaults, pdefault, map, default_para)
}

/// A style with everything it's based on folded in.
fn resolve(map: &BTreeMap<String, SDef>, id: &str) -> SDef {
    let mut chain = Vec::new();
    let mut cur = Some(String::from(id));
    while let Some(c) = cur {
        if chain.len() > 10 {
            break;
        }
        match map.get(&c) {
            Some(d) => {
                chain.push(d.clone());
                cur = d.based.clone();
            }
            None => break,
        }
    }
    let mut out = SDef::default();
    for d in chain.iter().rev() {
        out.rp.over(&d.rp);
        out.pp.over(&d.pp);
        if d.block.is_some() {
            out.block = d.block;
        }
    }
    if let Some(d) = map.get(id) {
        out.block = d.block;
    }
    out
}

/// numId -> (per level: is it a bullet list?).
fn read_numbering(b: &[u8]) -> BTreeMap<u32, [bool; 9]> {
    let mut abstracts: BTreeMap<u32, [bool; 9]> = BTreeMap::new();
    let mut nums: BTreeMap<u32, u32> = BTreeMap::new();
    let mut r = Reader::new(b);
    let mut cur_abs: Option<u32> = None;
    let mut cur_lvl = 0usize;
    let mut cur_num: Option<u32> = None;
    while let Some(ev) = r.next() {
        match ev {
            Ev::Start { name, attrs, .. } => match local(name) {
                "abstractNum" => {
                    let id = attr(&attrs, "abstractNumId").and_then(|v| v.parse().ok()).unwrap_or(0);
                    abstracts.insert(id, [true; 9]);
                    cur_abs = Some(id);
                }
                "lvl" => cur_lvl = attr(&attrs, "ilvl").and_then(|v| v.parse().ok()).unwrap_or(0usize).min(8),
                "numFmt" => {
                    if let Some(a) = cur_abs {
                        let bullet = matches!(attr(&attrs, "val"), Some("bullet") | Some("none"));
                        if let Some(l) = abstracts.get_mut(&a) {
                            l[cur_lvl] = bullet;
                        }
                    }
                }
                "num" => cur_num = attr(&attrs, "numId").and_then(|v| v.parse().ok()),
                "abstractNumId" => {
                    if let (Some(n), Some(a)) = (cur_num, attr(&attrs, "val").and_then(|v| v.parse().ok())) {
                        nums.insert(n, a);
                    }
                }
                _ => {}
            },
            Ev::End(name) => match local(name) {
                "abstractNum" => cur_abs = None,
                "num" => cur_num = None,
                _ => {}
            },
            _ => {}
        }
    }
    nums.into_iter().filter_map(|(n, a)| Some((n, *abstracts.get(&a)?))).collect()
}

/// Read a .docx. Also returns a note about anything left out.
pub fn read(b: &[u8]) -> Result<(Doc, Option<String>), String> {
    let files = zip::read(b).map_err(|e| format!("not a Word file ({})", e))?;
    let get = |n: &str| files.iter().find(|(name, _)| name.eq_ignore_ascii_case(n)).map(|(_, d)| d.as_slice());
    let body = get("word/document.xml").ok_or_else(|| String::from("not a Word file (no document inside)"))?;
    let (rdef, pdef, styles, default_para) = get("word/styles.xml").map(read_styles).unwrap_or_default();
    let numbering = get("word/numbering.xml").map(read_numbering).unwrap_or_default();

    let mut base = Style::NORMAL;
    base.size = 20; // Word's built-in default when nothing says otherwise
    rdef.apply(&mut base);
    let normal = default_para.as_deref().map(|id| resolve(&styles, id)).unwrap_or_default();
    normal.rp.apply(&mut base);
    let mut base_pp = PP { after: Some(0), line: Some(100), ..PP::default() };
    base_pp.over(&pdef);
    base_pp.over(&normal.pp);

    let mut doc = Doc::new();
    doc.paras.clear();
    let mut r = Reader::new(body);
    // State.
    let mut para: Option<(Para, PP, SDef)> = None;
    let mut ppr: Option<PP> = None;
    let mut run: Option<RP> = None;
    let mut in_rpr = false;
    let mut in_text = false;
    let mut skip = 0usize; // inside pictures, text boxes, deleted text, etc.
    // A picture being read: size in EMUs and the relationship id of its image.
    let mut pic: Option<(i64, i64, Option<String>)> = None;
    // Relationship id -> file in the zip.
    let mut rels: BTreeMap<String, String> = BTreeMap::new();
    if let Some(rb) = get("word/_rels/document.xml.rels") {
        let mut rr = Reader::new(rb);
        while let Some(ev) = rr.next() {
            if let Ev::Start { name, attrs, .. } = ev {
                if local(name) == "Relationship" {
                    if let (Some(id), Some(t)) = (attr(&attrs, "Id"), attr(&attrs, "Target")) {
                        let path = if let Some(abs) = t.strip_prefix('/') { String::from(abs) } else { format!("word/{}", t) };
                        rels.insert(String::from(id), path);
                    }
                }
            }
        }
    }
    let mut pictures = 0;
    // Tables: nesting depth, which table, column widths (points), the row being gathered.
    let mut tbl_depth = 0u32;
    let mut table_id = 0u32;
    let mut grid: Vec<i32> = Vec::new();
    let mut row_para: Option<Para> = None;
    let mut row_widths: Vec<i32> = Vec::new();
    let mut cell_w: Option<i32> = None;
    let mut cell_paras = 0usize;
    let mut rstyle: Option<String> = None;

    let para_style = |pp_id: &Option<String>| -> SDef {
        match pp_id {
            Some(id) => resolve(&styles, id),
            None => normal.clone(),
        }
    };

    fn finish(doc: &mut Doc, p: Option<(Para, PP, SDef)>, base_pp: &PP, numbering: &BTreeMap<u32, [bool; 9]>) {
        let Some((mut para, pp, sdef)) = p else { return };
        let mut all = *base_pp;
        all.over(&sdef.pp);
        all.over(&pp);
        let mut ps = PStyle::NORMAL;
        ps.align = all.align.unwrap_or(Align::Left);
        ps.after = all.after.unwrap_or(0);
        ps.line = all.line.unwrap_or(100);
        ps.block = sdef.block.unwrap_or(Block::Normal);
        if let Some((num, lvl)) = all.num.filter(|(n, _)| *n != 0) {
            let bullet = numbering.get(&num).map(|l| l[lvl as usize]).unwrap_or(true);
            ps.list = if bullet { List::Bullet } else { List::Number };
            ps.indent = lvl;
        } else {
            ps.indent = all.indent.unwrap_or(0);
        }
        para.p = ps;
        // Page breaks inside: split into separate paragraphs.
        if para.text.contains(&PAGE_BREAK) && !para.is_page_break() {
            let mut cur = Para::new(ps, para.end);
            for (ch, st) in para.text.iter().zip(&para.st) {
                if *ch == PAGE_BREAK {
                    doc.paras.push(core::mem::replace(&mut cur, Para::new(ps, para.end)));
                    doc.paras.push(Para { text: alloc::vec![PAGE_BREAK], st: alloc::vec![*st], p: PStyle::NORMAL, end: *st, row: None });
                } else {
                    cur.text.push(*ch);
                    cur.st.push(*st);
                }
            }
            doc.paras.push(cur);
        } else {
            doc.paras.push(para);
        }
    }

    while let Some(ev) = r.next() {
        match ev {
            Ev::Start { name, attrs, empty } => {
                let n = local(name);
                if skip > 0 {
                    // Inside a picture: note its size and which image file it shows.
                    if let Some(p) = &mut pic {
                        if n == "extent" && p.0 == 0 {
                            p.0 = attr(&attrs, "cx").and_then(|v| v.parse().ok()).unwrap_or(0);
                            p.1 = attr(&attrs, "cy").and_then(|v| v.parse().ok()).unwrap_or(0);
                        }
                        if n == "blip" && p.2.is_none() {
                            p.2 = attr(&attrs, "embed").map(String::from);
                        }
                    }
                    if !empty {
                        skip += 1;
                    }
                    continue;
                }
                match n {
                    "drawing" | "pict" | "object" | "AlternateContent" | "del" | "moveFrom" | "instrText" | "footnoteReference" | "commentReference" => {
                        if n == "drawing" && !empty {
                            pic = Some((0, 0, None));
                        } else if matches!(n, "pict" | "AlternateContent" | "object") {
                            pictures += 1;
                        }
                        if !empty {
                            skip = 1;
                        }
                    }
                    "p" => {
                        finish(&mut doc, para.take(), &base_pp, &numbering);
                        para = Some((Para::new(PStyle::NORMAL, base), PP::default(), normal.clone()));
                        if empty {
                            finish(&mut doc, para.take(), &base_pp, &numbering);
                        }
                    }
                    "pPr" if !empty && run.is_none() => ppr = Some(PP::default()),
                    "pStyle" if ppr.is_some() => {
                        let pstyle_id = attr(&attrs, "val").map(String::from);
                        if let Some((p, _, sdef)) = &mut para {
                            *sdef = para_style(&pstyle_id);
                            let mut st = base;
                            sdef.rp.apply(&mut st);
                            p.end = st;
                        }
                    }
                    "numPr" | "ilvl" | "numId" if ppr.is_some() => {
                        let pp = ppr.as_mut().unwrap();
                        let (mut num, mut lvl) = pp.num.unwrap_or((0, 0));
                        if n == "ilvl" {
                            lvl = attr(&attrs, "val").and_then(|v| v.parse().ok()).unwrap_or(0u8).min(8);
                        }
                        if n == "numId" {
                            num = attr(&attrs, "val").and_then(|v| v.parse().ok()).unwrap_or(0);
                        }
                        pp.num = Some((num, lvl));
                    }
                    "rPr" if !empty => in_rpr = true,
                    "r" => {
                        run = Some(RP::default());
                        rstyle = None;
                    }
                    "rStyle" if in_rpr => rstyle = attr(&attrs, "val").map(String::from),
                    "t" if !empty => in_text = true,
                    "tab" if run.is_some() => push_char(&mut para, '\t', &run, &rstyle, &styles, base),
                    "br" | "cr" if run.is_some() => {
                        let ch = if attr(&attrs, "type") == Some("page") { PAGE_BREAK } else { '\n' };
                        push_char(&mut para, ch, &run, &rstyle, &styles, base);
                    }
                    "lastRenderedPageBreak" => {}
                    "tbl" => {
                        tbl_depth += 1;
                        if tbl_depth == 1 {
                            table_id += 1;
                            grid.clear();
                        }
                    }
                    "gridCol" if tbl_depth == 1 => grid.push(attr(&attrs, "w").and_then(|v| v.parse::<i32>().ok()).unwrap_or(0) / 20),
                    "tr" if tbl_depth == 1 => {
                        let mut rp = Para::new(PStyle::NORMAL, base);
                        rp.end = base;
                        row_para = Some(rp);
                        row_widths.clear();
                    }
                    "tc" if tbl_depth == 1 => {
                        cell_paras = 0;
                        cell_w = None;
                    }
                    "tcW" if tbl_depth == 1 => {
                        if attr(&attrs, "type").is_none_or(|t| t == "dxa") {
                            cell_w = attr(&attrs, "w").and_then(|v| v.parse::<i32>().ok()).map(|w| w / 20).filter(|&w| w > 0);
                        }
                    }
                    "pgSz" => {
                        if let (Some(w), Some(h)) = (attr(&attrs, "w").and_then(|v| v.parse::<i32>().ok()), attr(&attrs, "h").and_then(|v| v.parse::<i32>().ok())) {
                            doc.page_w = (w / 20).clamp(144, 2000);
                            doc.page_h = (h / 20).clamp(144, 3000);
                        }
                    }
                    "pgMar" => {
                        for (k, key) in ["left", "top", "right", "bottom"].iter().enumerate() {
                            if let Some(v) = attr(&attrs, key).and_then(|v| v.parse::<i32>().ok()) {
                                doc.margin[k] = (v.abs() / 20).clamp(18, 216);
                            }
                        }
                    }
                    _ => {
                        if in_rpr {
                            if let Some(rp) = &mut run {
                                rp_child(rp, n, &attrs);
                            }
                        } else if let Some(pp) = &mut ppr {
                            pp_child(pp, n, &attrs);
                        }
                    }
                }
            }
            Ev::End(name) => {
                let n = local(name);
                if skip > 0 {
                    skip -= 1;
                    if skip == 0 {
                        if let Some((cx, cy, rid)) = pic.take() {
                            let data = rid.as_deref().and_then(|r| rels.get(r)).and_then(|t| get(t));
                            match data {
                                Some(bytes) if cx > 0 && cy > 0 => {
                                    let target = rels.get(rid.as_deref().unwrap_or("")).cloned().unwrap_or_default();
                                    let ext = target.rsplit('.').next().unwrap_or("png").to_ascii_lowercase();
                                    doc.images.push(DocImage { data: bytes.to_vec(), ext, w: ((cx / 12700) as i32).max(4), h: ((cy / 12700) as i32).max(4) });
                                    push_char(&mut para, PICTURE, &run, &rstyle, &styles, base);
                                    if let Some((p, _, _)) = &mut para {
                                        if let Some(st) = p.st.last_mut() {
                                            st.img = doc.images.len() as u16;
                                        }
                                    }
                                }
                                _ => pictures += 1,
                            }
                        }
                    }
                    continue;
                }
                match n {
                    "p" => {
                        // A paragraph in a table cell joins the row (a new line in the cell).
                        if let Some(rp) = row_para.as_mut() {
                            if let Some((cp, _, _)) = para.take() {
                                if cell_paras > 0 {
                                    rp.text.push('\n');
                                    rp.st.push(rp.end);
                                }
                                // Tabs inside a cell would split it: make them spaces.
                                rp.text.extend(cp.text.iter().map(|&c| if c == '\t' { ' ' } else { c }));
                                rp.st.extend(cp.st);
                                cell_paras += 1;
                            }
                        } else {
                            finish(&mut doc, para.take(), &base_pp, &numbering);
                        }
                    }
                    "tc" if tbl_depth == 1 => {
                        if let Some(rp) = row_para.as_mut() {
                            rp.text.push('\t');
                            rp.st.push(rp.end);
                            let w = cell_w.take().or_else(|| grid.get(row_widths.len()).copied()).unwrap_or(0);
                            row_widths.push(w);
                        }
                    }
                    "tr" if tbl_depth == 1 => {
                        if let Some(mut rp) = row_para.take() {
                            if rp.text.last() == Some(&'\t') {
                                rp.text.pop();
                                rp.st.pop();
                            }
                            rp.row = Some(super::doc::Row { table: table_id, widths: core::mem::take(&mut row_widths) });
                            rp.p.after = 0;
                            doc.paras.push(rp);
                        }
                    }
                    "tbl" => tbl_depth = tbl_depth.saturating_sub(1),
                    "pPr" => {
                        if let (Some(pp), Some((_, ppp, _))) = (ppr.take(), &mut para) {
                            *ppp = pp;
                        }
                    }
                    "rPr" => in_rpr = false,
                    "r" => run = None,
                    "t" => in_text = false,
                    _ => {}
                }
            }
            Ev::Text(t) => {
                if in_text && skip == 0 && run.is_some() {
                    for ch in t.chars() {
                        push_char(&mut para, ch, &run, &rstyle, &styles, base);
                    }
                }
            }
        }
    }
    finish(&mut doc, para.take(), &base_pp, &numbering);
    if doc.paras.is_empty() {
        doc.paras.push(Para::new(PStyle::NORMAL, base));
    }
    // Pictures wider or taller than the page area are shrunk to fit.
    let (cw, ch) = (doc.page_w - doc.margin[0] - doc.margin[2], doc.page_h - doc.margin[1] - doc.margin[3]);
    for im in doc.images.iter_mut() {
        if im.w > cw || im.h > ch {
            let (w0, h0) = (im.w, im.h);
            im.w = (w0 * cw / w0.max(1)).min(w0 * ch / h0.max(1)).max(4);
            im.h = (h0 * im.w / w0.max(1)).max(4);
        }
    }
    let mut notes = Vec::new();
    if pictures > 0 {
        notes.push(format!("{} drawing{} or text box{} not shown", pictures, if pictures == 1 { "" } else { "s" }, if pictures == 1 { "" } else { "es" }));
    }
    let note = if notes.is_empty() { None } else { Some(format!("Note: {} (saving here leaves them out)", notes.join(", "))) };
    Ok((doc, note))
}

fn push_char(para: &mut Option<(Para, PP, SDef)>, ch: char, run: &Option<RP>, rstyle: &Option<String>, styles: &BTreeMap<String, SDef>, base: Style) {
    let Some((p, _, sdef)) = para else { return };
    let mut st = base;
    sdef.rp.apply(&mut st);
    if let Some(id) = rstyle {
        resolve(styles, id).rp.apply(&mut st);
    }
    if let Some(rp) = run {
        rp.apply(&mut st);
    }
    if ch == '\r' {
        return;
    }
    p.text.push(ch);
    p.st.push(st);
}

// ---- writing ------------------------------------------------------------------------------

fn font_name(serif: bool) -> &'static str {
    if serif {
        "Times New Roman"
    } else {
        "Arial"
    }
}

fn rpr(s: &Style) -> String {
    let f = font_name(s.serif);
    let mut o = format!("<w:rPr><w:rFonts w:ascii=\"{0}\" w:hAnsi=\"{0}\" w:cs=\"{0}\"/>", f);
    if s.bold {
        o += "<w:b/><w:bCs/>";
    }
    if s.italic {
        o += "<w:i/><w:iCs/>";
    }
    if s.strike {
        o += "<w:strike/>";
    }
    if s.color != 0 {
        o += &format!("<w:color w:val=\"{:06X}\"/>", s.color);
    }
    o += &format!("<w:sz w:val=\"{0}\"/><w:szCs w:val=\"{0}\"/>", s.size);
    if let Some(h) = s.highlight {
        o += &format!("<w:shd w:val=\"clear\" w:color=\"auto\" w:fill=\"{:06X}\"/>", h);
    }
    if s.underline {
        o += "<w:u w:val=\"single\"/>";
    }
    o += "</w:rPr>";
    o
}

fn style_xml(id: &str, name: &str, b: Block) -> String {
    let s = b.style(Style::NORMAL);
    let space = if b.space_before() > 0 { format!("<w:spacing w:before=\"{}\" w:after=\"80\"/>", b.space_before() * 20) } else { String::new() };
    format!(
        "<w:style w:type=\"paragraph\" w:styleId=\"{id}\"><w:name w:val=\"{name}\"/><w:basedOn w:val=\"Normal\"/><w:next w:val=\"Normal\"/><w:qFormat/><w:pPr><w:keepNext/>{space}</w:pPr>{}</w:style>",
        rpr(&s)
    )
}

pub fn write(doc: &Doc) -> Vec<u8> {
    let mut body = String::from(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n<w:document xmlns:w=\"http://schemas.openxmlformats.org/wordprocessingml/2006/main\" xmlns:r=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships\" xmlns:wp=\"http://schemas.openxmlformats.org/drawingml/2006/wordprocessingDrawing\"><w:body>",
    );
    // Each run of numbered paragraphs gets its own numbering (so it starts at 1).
    let mut next_num = 3u32;
    let mut used_images: alloc::collections::BTreeSet<usize> = alloc::collections::BTreeSet::new();
    let mut pic_id = 0u32;
    let mut num_runs: Vec<u32> = Vec::new();
    let mut in_numbers = false;
    for (pk, p) in doc.paras.iter().enumerate() {
        if p.is_page_break() {
            body += "<w:p><w:r><w:br w:type=\"page\"/></w:r></w:p>";
            in_numbers = false;
            continue;
        }
        if let Some(row) = &p.row {
            in_numbers = false;
            let same = |q: Option<&Para>| q.and_then(|q| q.row.as_ref()).map(|r| r.table) == Some(row.table);
            let prev_same = pk > 0 && same(doc.paras.get(pk - 1));
            let next_same = same(doc.paras.get(pk + 1));
            // Cells: split at the tabs.
            let mut cells = Vec::new();
            let mut s = 0;
            for (i, &ch) in p.text.iter().enumerate() {
                if ch == '\t' {
                    cells.push((s, i));
                    s = i + 1;
                }
            }
            cells.push((s, p.text.len()));
            let cw = doc.page_w - doc.margin[0] - doc.margin[2];
            let widths: Vec<i32> = (0..cells.len()).map(|k| row.widths.get(k).copied().filter(|&w| w > 0).unwrap_or(cw / cells.len() as i32)).collect();
            if !prev_same {
                body += "<w:tbl><w:tblPr><w:tblW w:w=\"0\" w:type=\"auto\"/><w:tblBorders>";
                for side in ["top", "left", "bottom", "right", "insideH", "insideV"] {
                    body += &format!("<w:{} w:val=\"single\" w:sz=\"4\" w:space=\"0\" w:color=\"808080\"/>", side);
                }
                body += "</w:tblBorders><w:tblLayout w:type=\"fixed\"/></w:tblPr><w:tblGrid>";
                for w in &widths {
                    body += &format!("<w:gridCol w:w=\"{}\"/>", w * 20);
                }
                body += "</w:tblGrid>";
            }
            body += "<w:tr>";
            for (k, &(s, e)) in cells.iter().enumerate() {
                body += &format!("<w:tc><w:tcPr><w:tcW w:w=\"{}\" w:type=\"dxa\"/></w:tcPr><w:p><w:pPr><w:spacing w:after=\"0\"/></w:pPr>", widths[k] * 20);
                write_runs(doc, &p.text[s..e], &p.st[s..e], &mut body, &mut used_images, &mut pic_id);
                body += "</w:p></w:tc>";
            }
            body += "</w:tr>";
            if !next_same {
                body += "</w:tbl>";
            }
            continue;
        }
        body += "<w:p><w:pPr>";
        match p.p.block {
            Block::Normal => {}
            Block::Title => body += "<w:pStyle w:val=\"Title\"/>",
            Block::H1 => body += "<w:pStyle w:val=\"Heading1\"/>",
            Block::H2 => body += "<w:pStyle w:val=\"Heading2\"/>",
            Block::H3 => body += "<w:pStyle w:val=\"Heading3\"/>",
        }
        match p.p.list {
            List::Bullet => {
                in_numbers = false;
                body += &format!("<w:numPr><w:ilvl w:val=\"{}\"/><w:numId w:val=\"1\"/></w:numPr>", p.p.indent);
            }
            List::Number => {
                if !in_numbers {
                    num_runs.push(next_num);
                    next_num += 1;
                    in_numbers = true;
                }
                body += &format!("<w:numPr><w:ilvl w:val=\"{}\"/><w:numId w:val=\"{}\"/></w:numPr>", p.p.indent, num_runs.last().unwrap());
            }
            List::None => {
                in_numbers = false;
                if p.p.indent > 0 {
                    body += &format!("<w:ind w:left=\"{}\"/>", p.p.indent as u32 * 720);
                }
            }
        }
        body += &format!("<w:spacing w:after=\"{}\" w:line=\"{}\" w:lineRule=\"auto\"/>", p.p.after as u32 * 20, p.p.line as u32 * 240 / 100);
        let jc = match p.p.align {
            Align::Left => "",
            Align::Center => "center",
            Align::Right => "right",
            Align::Justify => "both",
        };
        if !jc.is_empty() {
            body += &format!("<w:jc w:val=\"{}\"/>", jc);
        }
        body += &rpr(&p.end).replace("w:rPr", "w:rPr");
        body += "</w:pPr>";
        write_runs(doc, &p.text, &p.st, &mut body, &mut used_images, &mut pic_id);
        body += "</w:p>";
    }
    body += &format!(
        "<w:sectPr><w:pgSz w:w=\"{}\" w:h=\"{}\"/><w:pgMar w:top=\"{}\" w:right=\"{}\" w:bottom=\"{}\" w:left=\"{}\" w:header=\"720\" w:footer=\"720\" w:gutter=\"0\"/></w:sectPr></w:body></w:document>",
        doc.page_w * 20,
        doc.page_h * 20,
        doc.margin[1] * 20,
        doc.margin[2] * 20,
        doc.margin[3] * 20,
        doc.margin[0] * 20
    );

    let styles = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n<w:styles xmlns:w=\"http://schemas.openxmlformats.org/wordprocessingml/2006/main\"><w:docDefaults><w:rPrDefault><w:rPr><w:rFonts w:ascii=\"Arial\" w:hAnsi=\"Arial\" w:cs=\"Arial\"/><w:sz w:val=\"22\"/><w:szCs w:val=\"22\"/></w:rPr></w:rPrDefault><w:pPrDefault><w:pPr><w:spacing w:after=\"160\" w:line=\"240\" w:lineRule=\"auto\"/></w:pPr></w:pPrDefault></w:docDefaults><w:style w:type=\"paragraph\" w:default=\"1\" w:styleId=\"Normal\"><w:name w:val=\"Normal\"/><w:qFormat/></w:style>{}{}{}{}<w:style w:type=\"paragraph\" w:styleId=\"ListParagraph\"><w:name w:val=\"List Paragraph\"/><w:basedOn w:val=\"Normal\"/><w:pPr><w:ind w:left=\"720\"/></w:pPr></w:style></w:styles>",
        style_xml("Title", "Title", Block::Title),
        style_xml("Heading1", "heading 1", Block::H1),
        style_xml("Heading2", "heading 2", Block::H2),
        style_xml("Heading3", "heading 3", Block::H3)
    );

    let mut numbering = String::from("<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n<w:numbering xmlns:w=\"http://schemas.openxmlformats.org/wordprocessingml/2006/main\">");
    for (id, bullet) in [(0, true), (1, false)] {
        numbering += &format!("<w:abstractNum w:abstractNumId=\"{}\"><w:multiLevelType w:val=\"hybridMultilevel\"/>", id);
        for l in 0..9u32 {
            let (fmt, text) = if bullet { ("bullet", String::from(if l % 2 == 1 { "\u{25E6}" } else { "\u{2022}" })) } else { ("decimal", format!("%{}.", l + 1)) };
            numbering += &format!(
                "<w:lvl w:ilvl=\"{l}\"><w:start w:val=\"1\"/><w:numFmt w:val=\"{fmt}\"/><w:lvlText w:val=\"{text}\"/><w:lvlJc w:val=\"left\"/><w:pPr><w:ind w:left=\"{}\" w:hanging=\"360\"/></w:pPr></w:lvl>",
                720 * l + 360
            );
        }
        numbering += "</w:abstractNum>";
    }
    numbering += "<w:num w:numId=\"1\"><w:abstractNumId w:val=\"0\"/></w:num><w:num w:numId=\"2\"><w:abstractNumId w:val=\"1\"/></w:num>";
    for n in &num_runs {
        numbering += &format!("<w:num w:numId=\"{}\"><w:abstractNumId w:val=\"1\"/><w:lvlOverride w:ilvl=\"0\"><w:startOverride w:val=\"1\"/></w:lvlOverride></w:num>", n);
    }
    numbering += "</w:numbering>";

    let types = "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n<Types xmlns=\"http://schemas.openxmlformats.org/package/2006/content-types\"><Default Extension=\"rels\" ContentType=\"application/vnd.openxmlformats-package.relationships+xml\"/><Default Extension=\"xml\" ContentType=\"application/xml\"/><Override PartName=\"/word/document.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml\"/><Override PartName=\"/word/styles.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.wordprocessingml.styles+xml\"/><Override PartName=\"/word/numbering.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.wordprocessingml.numbering+xml\"/></Types>";
    let rels = "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n<Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\"><Relationship Id=\"rId1\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument\" Target=\"word/document.xml\"/></Relationships>";
    // Pictures: their files, how to find them, and their kinds.
    let mut img_rels = String::new();
    let mut media: Vec<(String, &[u8])> = Vec::new();
    let mut exts: alloc::collections::BTreeSet<String> = alloc::collections::BTreeSet::new();
    for &k in &used_images {
        let im = &doc.images[k - 1];
        let name = format!("media/image{}.{}", k, im.ext);
        img_rels += &format!("<Relationship Id=\"rIdImg{}\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/image\" Target=\"{}\"/>", k, name);
        media.push((format!("word/{}", name), &im.data));
        exts.insert(im.ext.clone());
    }
    let mut type_defaults = String::new();
    for e in &exts {
        let mime = match e.as_str() {
            "png" => "image/png",
            "jpg" | "jpeg" => "image/jpeg",
            "gif" => "image/gif",
            "bmp" => "image/bmp",
            "tif" | "tiff" => "image/tiff",
            "emf" => "image/x-emf",
            "wmf" => "image/x-wmf",
            "svg" => "image/svg+xml",
            _ => "application/octet-stream",
        };
        type_defaults += &format!("<Default Extension=\"{}\" ContentType=\"{}\"/>", e, mime);
    }
    let types = types.replacen("<Default Extension=\"xml\"", &format!("{}<Default Extension=\"xml\"", type_defaults), 1);
    let doc_rels = format!("<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n<Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\"><Relationship Id=\"rId1\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/styles\" Target=\"styles.xml\"/><Relationship Id=\"rId2\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/numbering\" Target=\"numbering.xml\"/>{}</Relationships>", img_rels);
    let mut files: Vec<(&str, &[u8])> = Vec::from([
        ("[Content_Types].xml", types.as_bytes()),
        ("_rels/.rels", rels.as_bytes()),
        ("word/document.xml", body.as_bytes()),
        ("word/styles.xml", styles.as_bytes()),
        ("word/numbering.xml", numbering.as_bytes()),
        ("word/_rels/document.xml.rels", doc_rels.as_bytes()),
    ]);
    for (n, d) in &media {
        files.push((n.as_str(), d));
    }
    zip::write(&files)
}

/// The runs (text, tabs, breaks, pictures) of one paragraph or table cell.
fn write_runs(doc: &Doc, text: &[char], sts: &[Style], body: &mut String, used_images: &mut alloc::collections::BTreeSet<usize>, pic_id: &mut u32) {
        let mut i = 0;
        while i < text.len() {
            let st = sts[i];
            let mut j = i;
            while j < text.len() && sts[j] == st {
                j += 1;
            }
            if st.img != 0 {
                if let Some(im) = doc.image_of(&st) {
                    let k = st.img as usize;
                    used_images.insert(k);
                    let (cx, cy) = (im.w as i64 * 12700, im.h as i64 * 12700);
                    for _ in i..j {
                        *pic_id += 1;
                        *body += &format!(
                            "<w:r><w:drawing><wp:inline distT=\"0\" distB=\"0\" distL=\"0\" distR=\"0\"><wp:extent cx=\"{cx}\" cy=\"{cy}\"/><wp:docPr id=\"{pic_id}\" name=\"Picture {pic_id}\"/><a:graphic xmlns:a=\"http://schemas.openxmlformats.org/drawingml/2006/main\"><a:graphicData uri=\"http://schemas.openxmlformats.org/drawingml/2006/picture\"><pic:pic xmlns:pic=\"http://schemas.openxmlformats.org/drawingml/2006/picture\"><pic:nvPicPr><pic:cNvPr id=\"{pic_id}\" name=\"image{k}.{ext}\"/><pic:cNvPicPr/></pic:nvPicPr><pic:blipFill><a:blip r:embed=\"rIdImg{k}\"/><a:stretch><a:fillRect/></a:stretch></pic:blipFill><pic:spPr><a:xfrm><a:off x=\"0\" y=\"0\"/><a:ext cx=\"{cx}\" cy=\"{cy}\"/></a:xfrm><a:prstGeom prst=\"rect\"><a:avLst/></a:prstGeom></pic:spPr></pic:pic></a:graphicData></a:graphic></wp:inline></w:drawing></w:r>",
                            ext = im.ext
                        );
                    }
                }
                i = j;
                continue;
            }
            *body += "<w:r>";
            *body += &rpr(&st);
            let mut chunk = String::new();
            let flush = |chunk: &mut String, body: &mut String| {
                if !chunk.is_empty() {
                    *body += &format!("<w:t xml:space=\"preserve\">{}</w:t>", escape(chunk));
                    chunk.clear();
                }
            };
            for &ch in &text[i..j] {
                match ch {
                    '\t' => {
                        flush(&mut chunk, body);
                        *body += "<w:tab/>";
                    }
                    '\n' => {
                        flush(&mut chunk, body);
                        *body += "<w:br/>";
                    }
                    PAGE_BREAK => {
                        flush(&mut chunk, body);
                        *body += "<w:br w:type=\"page\"/>";
                    }
                    c => chunk.push(c),
                }
            }
            flush(&mut chunk, body);
            *body += "</w:r>";
            i = j;
        }
}
