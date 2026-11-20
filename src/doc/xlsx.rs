//! Excel .xlsx files: read values, formulas, styles (bold, italic, colours, number
//! formats, alignment), column widths and every sheet; write them back.
//! Charts, pictures, merged cells and other extras aren't kept.

use alloc::collections::BTreeMap;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use super::sheet::{parse_cell, Cell, Content, NumFmt, Sheet, Style, Value, Workbook};
use super::xml::{attr, escape, local, Ev, Reader};
use super::zip;

fn member<'a>(files: &'a [(String, Vec<u8>)], path: &str) -> Option<&'a [u8]> {
    let path = path.trim_start_matches('/');
    files.iter().find(|(n, _)| n.eq_ignore_ascii_case(path)).map(|(_, d)| d.as_slice())
}

/// "FF336699" / "336699" -> 0x336699.
fn argb(s: &str) -> Option<u32> {
    let h = if s.len() == 8 { &s[2..] } else { s };
    u32::from_str_radix(h, 16).ok()
}

/// Excel's built-in number formats (the ones that matter).
fn builtin_fmt(id: u32) -> Option<&'static str> {
    Some(match id {
        0 => "General",
        1 => "0",
        2 => "0.00",
        3 => "#,##0",
        4 => "#,##0.00",
        9 => "0%",
        10 => "0.00%",
        11 => "0.00E+00",
        14 => "m/d/yyyy",
        15 => "d-mmm-yy",
        16 => "d-mmm",
        17 => "mmm-yy",
        22 => "m/d/yyyy h:mm",
        37 => "#,##0 ;(#,##0)",
        38 => "#,##0 ;[Red](#,##0)",
        39 => "#,##0.00;(#,##0.00)",
        40 => "#,##0.00;[Red](#,##0.00)",
        44 => "_(\"$\"* #,##0.00_)",
        49 => "@",
        _ => return None,
    })
}

#[derive(Default, Clone)]
struct Font {
    bold: bool,
    italic: bool,
    color: Option<u32>,
}

fn parse_styles(data: &[u8]) -> Vec<Style> {
    let mut numfmts: BTreeMap<u32, String> = BTreeMap::new();
    let mut fonts: Vec<Font> = Vec::new();
    let mut fills: Vec<Option<u32>> = Vec::new();
    let mut xfs: Vec<Style> = Vec::new();
    let mut r = Reader::new(data);
    let mut section = "";
    let mut font = Font::default();
    let mut fill: Option<u32> = None;
    let mut in_xf: Option<Style> = None;
    while let Some(ev) = r.next() {
        match ev {
            Ev::Start { name, attrs, empty } => match local(name) {
                "numFmt" => {
                    if let (Some(id), Some(code)) = (attr(&attrs, "numFmtId").and_then(|v| v.parse().ok()), attr(&attrs, "formatCode")) {
                        numfmts.insert(id, String::from(code));
                    }
                }
                "fonts" => section = "fonts",
                "fills" => section = "fills",
                "cellXfs" => section = "cellXfs",
                "cellStyleXfs" => section = "other",
                "font" if section == "fonts" => {
                    font = Font::default();
                    if empty {
                        fonts.push(font.clone());
                    }
                }
                "b" if section == "fonts" => font.bold = attr(&attrs, "val").is_none_or(|v| v != "0" && v != "false"),
                "i" if section == "fonts" => font.italic = attr(&attrs, "val").is_none_or(|v| v != "0" && v != "false"),
                "color" if section == "fonts" => font.color = attr(&attrs, "rgb").and_then(argb),
                "fill" if section == "fills" => {
                    fill = None;
                    if empty {
                        fills.push(None);
                    }
                }
                "patternFill" if section == "fills" => {
                    if empty {
                        fill = None;
                    }
                }
                "fgColor" if section == "fills" => fill = attr(&attrs, "rgb").and_then(argb),
                "xf" if section == "cellXfs" => {
                    let font_id: usize = attr(&attrs, "fontId").and_then(|v| v.parse().ok()).unwrap_or(0);
                    let fill_id: usize = attr(&attrs, "fillId").and_then(|v| v.parse().ok()).unwrap_or(0);
                    let fmt_id: u32 = attr(&attrs, "numFmtId").and_then(|v| v.parse().ok()).unwrap_or(0);
                    let f = fonts.get(font_id).cloned().unwrap_or_default();
                    let code = numfmts.get(&fmt_id).cloned().or_else(|| builtin_fmt(fmt_id).map(String::from)).unwrap_or_else(|| String::from("General"));
                    let st = Style {
                        bold: f.bold,
                        italic: f.italic,
                        color: f.color.filter(|&c| c != 0),
                        fill: fills.get(fill_id).copied().flatten(),
                        fmt: if code == "@" { NumFmt::General } else { NumFmt::from_code(&code) },
                        align: 0,
                    };
                    if empty {
                        xfs.push(st);
                    } else {
                        in_xf = Some(st);
                    }
                }
                "alignment" => {
                    if let Some(st) = &mut in_xf {
                        st.align = match attr(&attrs, "horizontal") {
                            Some("left") => -1,
                            Some("center") | Some("centerContinuous") => 1,
                            Some("right") => 2,
                            _ => 0,
                        };
                    }
                }
                _ => {}
            },
            Ev::End(name) => match local(name) {
                "font" if section == "fonts" => fonts.push(font.clone()),
                "fill" if section == "fills" => fills.push(fill),
                "xf" => {
                    if let Some(st) = in_xf.take() {
                        xfs.push(st);
                    }
                }
                "fonts" | "fills" | "cellXfs" | "cellStyleXfs" => section = "",
                _ => {}
            },
            Ev::Text(_) => {}
        }
    }
    xfs
}

fn parse_shared(data: &[u8]) -> Vec<String> {
    let mut out = Vec::new();
    let mut r = Reader::new(data);
    let mut cur: Option<String> = None;
    let mut in_t = false;
    let mut in_rph = false;
    while let Some(ev) = r.next() {
        match ev {
            Ev::Start { name, empty, .. } => match local(name) {
                "si" => {
                    cur = Some(String::new());
                    if empty {
                        out.push(String::new());
                        cur = None;
                    }
                }
                "t" if !empty => in_t = true,
                "rPh" => in_rph = !empty, // phonetic hints aren't part of the text
                _ => {}
            },
            Ev::End(name) => match local(name) {
                "si" => out.push(cur.take().unwrap_or_default()),
                "t" => in_t = false,
                "rPh" => in_rph = false,
                _ => {}
            },
            Ev::Text(t) => {
                if in_t && !in_rph {
                    if let Some(c) = &mut cur {
                        c.push_str(&t);
                    }
                }
            }
        }
    }
    out
}

fn parse_sheet(name: &str, data: &[u8], shared: &[String], styles: &[Style]) -> Sheet {
    let mut sh = Sheet::new(name);
    let mut r = Reader::new(data);
    // Current cell: (row, col), type, style index
    let mut cell: Option<((u32, u32), String, usize)> = None;
    let (mut v, mut f, mut is_text) = (None::<String>, None::<String>, String::new());
    let mut what = "";
    let mut shared_f: BTreeMap<String, (String, (u32, u32))> = BTreeMap::new();
    let mut next_col = 0u32;
    let mut row = 0u32;
    let mut f_attrs: (Option<String>, Option<String>) = (None, None);
    while let Some(ev) = r.next() {
        match ev {
            Ev::Start { name, attrs, empty } => match local(name) {
                "col" => {
                    let min: u32 = attr(&attrs, "min").and_then(|v| v.parse().ok()).unwrap_or(1);
                    let max: u32 = attr(&attrs, "max").and_then(|v| v.parse().ok()).unwrap_or(min);
                    if let Some(w) = attr(&attrs, "width").and_then(|v| v.parse::<f32>().ok()) {
                        if attr(&attrs, "customWidth").is_some() || (w - super::sheet::DEFAULT_WIDTH).abs() > 0.5 {
                            for c in min..=max.min(min + 200) {
                                sh.widths.insert(c - 1, (w - 0.71).max(0.5));
                            }
                        }
                    }
                }
                "row" => {
                    row = attr(&attrs, "r").and_then(|v| v.parse::<u32>().ok()).map(|r| r - 1).unwrap_or(row);
                    next_col = 0;
                }
                "c" => {
                    let pos = attr(&attrs, "r").and_then(parse_cell).unwrap_or((row, next_col));
                    next_col = pos.1 + 1;
                    let t = String::from(attr(&attrs, "t").unwrap_or("n"));
                    let s = attr(&attrs, "s").and_then(|v| v.parse().ok()).unwrap_or(0);
                    v = None;
                    f = None;
                    is_text.clear();
                    if empty {
                        // Formatted but empty.
                        let st = styles.get(s).cloned().unwrap_or_default();
                        if st != Style::default() {
                            sh.styles.insert(pos, st);
                        }
                    } else {
                        cell = Some((pos, t, s));
                    }
                }
                "v" if !empty => what = "v",
                "f" => {
                    f_attrs = (attr(&attrs, "t").map(String::from), attr(&attrs, "si").map(String::from));
                    if empty {
                        // Uses a shared formula defined elsewhere.
                        if let (Some(si), Some((pos, _, _))) = (&f_attrs.1, &cell) {
                            if let Some((text, origin)) = shared_f.get(si) {
                                f = Some(shift_formula(text, pos.0 as i64 - origin.0 as i64, pos.1 as i64 - origin.1 as i64));
                            }
                        }
                    } else {
                        what = "f";
                        f = Some(String::new());
                    }
                }
                "t" if !empty && cell.is_some() => what = "t",
                _ => {}
            },
            Ev::Text(t) => match what {
                "v" => v = Some(v.unwrap_or_default() + &t),
                "f" => {
                    if let Some(x) = &mut f {
                        x.push_str(&t);
                    }
                }
                "t" => is_text.push_str(&t),
                _ => {}
            },
            Ev::End(name) => match local(name) {
                "v" | "t" => what = "",
                "f" => {
                    what = "";
                    if let (Some(text), Some(si), Some((pos, _, _))) = (&f, &f_attrs.1, &cell) {
                        if f_attrs.0.as_deref() == Some("shared") && !text.is_empty() {
                            shared_f.insert(si.clone(), (text.clone(), *pos));
                        }
                    }
                }
                "c" => {
                    let Some((pos, t, s)) = cell.take() else { continue };
                    let style = styles.get(s).cloned().unwrap_or_default();
                    let raw = v.take().unwrap_or_default();
                    let value = match t.as_str() {
                        "s" => raw.trim().parse::<usize>().ok().and_then(|i| shared.get(i)).map(|s| Value::Text(s.clone())),
                        "inlineStr" => Some(Value::Text(core::mem::take(&mut is_text))),
                        "str" => Some(Value::Text(raw)),
                        "b" => Some(Value::Bool(raw.trim() == "1")),
                        "e" => Some(Value::Err(err_static(raw.trim()))),
                        _ => raw.trim().parse::<f64>().ok().map(Value::Num),
                    };
                    let content = match f.take().filter(|x| !x.is_empty()) {
                        Some(formula) => Some(Content::Formula(formula, value)),
                        None => match value {
                            Some(Value::Num(n)) => Some(Content::Num(n)),
                            Some(Value::Text(x)) => Some(Content::Text(x)),
                            Some(Value::Bool(b)) => Some(Content::Bool(b)),
                            Some(Value::Err(e)) => Some(Content::Text(String::from(e))),
                            _ => None,
                        },
                    };
                    match content {
                        Some(content) => {
                            sh.cells.insert(pos, Cell { content, style });
                        }
                        None if style != Style::default() => {
                            sh.styles.insert(pos, style);
                        }
                        None => {}
                    }
                }
                _ => {}
            },
        }
    }
    sh
}

fn err_static(s: &str) -> &'static str {
    ["#DIV/0!", "#N/A", "#NAME?", "#NULL!", "#NUM!", "#REF!", "#VALUE!"].into_iter().find(|e| *e == s).unwrap_or("#VALUE!")
}

/// Move the relative references of a shared formula by (dr, dc).
pub fn shift_formula(f: &str, dr: i64, dc: i64) -> String {
    let mut out = String::new();
    let b: Vec<char> = f.chars().collect();
    let mut i = 0;
    let mut in_str = false;
    while i < b.len() {
        let c = b[i];
        if c == '"' {
            in_str = !in_str;
            out.push(c);
            i += 1;
            continue;
        }
        let prev_ok = i == 0 || !(b[i - 1].is_ascii_alphanumeric() || b[i - 1] == '_' || b[i - 1] == '.');
        if !in_str && prev_ok && (c == '$' || c.is_ascii_alphabetic()) {
            // Try to read [$]LETTERS[$]DIGITS
            let mut j = i;
            let abs_c = b[j] == '$';
            if abs_c {
                j += 1;
            }
            let ls = j;
            while j < b.len() && b[j].is_ascii_alphabetic() {
                j += 1;
            }
            let letters: String = b[ls..j].iter().collect();
            let abs_r = j < b.len() && b[j] == '$';
            if abs_r {
                j += 1;
            }
            let ds = j;
            while j < b.len() && b[j].is_ascii_digit() {
                j += 1;
            }
            let digits: String = b[ds..j].iter().collect();
            let followed_ok = j >= b.len() || !(b[j].is_ascii_alphanumeric() || b[j] == '(' || b[j] == '_');
            if !letters.is_empty() && letters.len() <= 3 && !digits.is_empty() && followed_ok {
                if let Some((r, col)) = parse_cell(&format!("{}{}", letters, digits)) {
                    let nr = if abs_r { r as i64 } else { r as i64 + dr };
                    let nc = if abs_c { col as i64 } else { col as i64 + dc };
                    if nr >= 0 && nc >= 0 {
                        out.push_str(&format!(
                            "{}{}{}{}",
                            if abs_c { "$" } else { "" },
                            super::sheet::col_name(nc as u32),
                            if abs_r { "$" } else { "" },
                            nr + 1
                        ));
                        i = j;
                        continue;
                    }
                }
            }
            // Not a reference: copy the word as is.
            let mut k = i;
            while k < b.len() && (b[k].is_ascii_alphanumeric() || b[k] == '$' || b[k] == '_' || b[k] == '.') {
                out.push(b[k]);
                k += 1;
            }
            i = k.max(i + 1);
            continue;
        }
        out.push(c);
        i += 1;
    }
    out
}

pub fn read(bytes: &[u8]) -> Result<Workbook, &'static str> {
    let files = zip::read(bytes)?;
    let wb_xml = member(&files, "xl/workbook.xml").ok_or("Not an Excel workbook")?;
    // Relationship id -> target
    let mut rels: BTreeMap<String, String> = BTreeMap::new();
    if let Some(d) = member(&files, "xl/_rels/workbook.xml.rels") {
        let mut r = Reader::new(d);
        while let Some(ev) = r.next() {
            if let Ev::Start { name, attrs, .. } = ev {
                if local(name) == "Relationship" {
                    if let (Some(id), Some(t)) = (attr(&attrs, "Id"), attr(&attrs, "Target")) {
                        let path = if let Some(abs) = t.strip_prefix('/') { String::from(abs) } else { format!("xl/{}", t) };
                        rels.insert(String::from(id), path);
                    }
                }
            }
        }
    }
    let mut sheet_list = Vec::new();
    let mut r = Reader::new(wb_xml);
    while let Some(ev) = r.next() {
        if let Ev::Start { name, attrs, .. } = ev {
            if local(name) == "sheet" {
                let sname = String::from(attr(&attrs, "name").unwrap_or("Sheet"));
                let rid = attrs.iter().find(|(k, _)| k.ends_with(":id") || *k == "id").map(|(_, v)| v.clone()).unwrap_or_default();
                sheet_list.push((sname, rid));
            }
        }
    }
    let shared = member(&files, "xl/sharedStrings.xml").map(parse_shared).unwrap_or_default();
    let styles = member(&files, "xl/styles.xml").map(parse_styles).unwrap_or_default();
    let mut wb = Workbook { sheets: Vec::new(), values: BTreeMap::new() };
    for (i, (name, rid)) in sheet_list.iter().enumerate() {
        let path = rels.get(rid).cloned().unwrap_or_else(|| format!("xl/worksheets/sheet{}.xml", i + 1));
        // Chart sheets and the like have no worksheet: skip them.
        let Some(data) = member(&files, &path) else { continue };
        wb.sheets.push(parse_sheet(name, data, &shared, &styles));
    }
    if wb.sheets.is_empty() {
        return Err("The workbook has no sheets Solon can show");
    }
    wb.recalc();
    Ok(wb)
}

// ---- writing --------------------------------------------------------------------------

#[derive(PartialEq, Clone)]
struct XfKey {
    font: usize,
    fill: usize,
    fmt: u32,
    align: i8,
}

pub fn write(wb: &Workbook) -> Vec<u8> {
    // Collect fonts, fills, number formats and cell formats in use.
    let mut fonts: Vec<(bool, bool, Option<u32>)> = Vec::from([(false, false, None)]);
    let mut fills: Vec<Option<u32>> = Vec::from([None, None]); // 0 none, 1 gray125 (required)
    let mut fmts: Vec<String> = Vec::new(); // custom codes, ids from 164
    let mut xfs: Vec<XfKey> = Vec::from([XfKey { font: 0, fill: 0, fmt: 0, align: 0 }]);
    let mut xf_of = |st: &Style| -> usize {
        let fk = (st.bold, st.italic, st.color);
        let font = fonts.iter().position(|f| *f == fk).unwrap_or_else(|| {
            fonts.push(fk);
            fonts.len() - 1
        });
        let fill = match st.fill {
            None => 0,
            Some(c) => fills.iter().skip(2).position(|f| *f == Some(c)).map(|p| p + 2).unwrap_or_else(|| {
                fills.push(Some(c));
                fills.len() - 1
            }),
        };
        let code = st.fmt.code();
        let fmt = match code.as_str() {
            "General" => 0,
            "0" => 1,
            "0.00" => 2,
            "#,##0" => 3,
            "#,##0.00" => 4,
            "0%" => 9,
            "0.00%" => 10,
            "m/d/yyyy" => 14,
            _ => {
                164 + fmts.iter().position(|f| *f == code).unwrap_or_else(|| {
                    fmts.push(code.clone());
                    fmts.len() - 1
                }) as u32
            }
        };
        let key = XfKey { font, fill, fmt, align: st.align };
        xfs.iter().position(|x| *x == key).unwrap_or_else(|| {
            xfs.push(key);
            xfs.len() - 1
        })
    };

    let mut sheet_xml = Vec::new();
    for sh in &wb.sheets {
        let mut x = String::from("<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n<worksheet xmlns=\"http://schemas.openxmlformats.org/spreadsheetml/2006/main\" xmlns:r=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships\">");
        if !sh.widths.is_empty() {
            x += "<cols>";
            for (c, w) in &sh.widths {
                x += &format!("<col min=\"{0}\" max=\"{0}\" width=\"{1:.2}\" customWidth=\"1\"/>", c + 1, w + 0.71);
            }
            x += "</cols>";
        }
        x += "<sheetData>";
        // Rows: cells with content plus formatted empty cells.
        let mut all: BTreeMap<(u32, u32), Option<&Cell>> = BTreeMap::new();
        for (k, c) in &sh.cells {
            all.insert(*k, Some(c));
        }
        for k in sh.styles.keys() {
            all.entry(*k).or_insert(None);
        }
        let mut cur_row: Option<u32> = None;
        for ((r, c), cell) in &all {
            if cur_row != Some(*r) {
                if cur_row.is_some() {
                    x += "</row>";
                }
                x += &format!("<row r=\"{}\">", r + 1);
                cur_row = Some(*r);
            }
            let st = match cell {
                Some(cell) => cell.style.clone(),
                None => sh.styles.get(&(*r, *c)).cloned().unwrap_or_default(),
            };
            let s = xf_of(&st);
            let sattr = if s == 0 { String::new() } else { format!(" s=\"{}\"", s) };
            let name = super::sheet::cell_name(*r, *c);
            match cell.map(|c| &c.content) {
                None => x += &format!("<c r=\"{}\"{}/>", name, sattr),
                Some(Content::Num(n)) => x += &format!("<c r=\"{}\"{}><v>{}</v></c>", name, sattr, num_text(*n)),
                Some(Content::Bool(b)) => x += &format!("<c r=\"{}\"{} t=\"b\"><v>{}</v></c>", name, sattr, *b as u8),
                Some(Content::Text(t)) => {
                    x += &format!("<c r=\"{}\"{} t=\"inlineStr\"><is><t xml:space=\"preserve\">{}</t></is></c>", name, sattr, escape(t))
                }
                Some(Content::Formula(f, _)) => {
                    let si = wb.sheets.iter().position(|q| core::ptr::eq(q, sh)).unwrap_or(0);
                    let (t, v) = match wb.values.get(&(si, *r, *c)) {
                        Some(Value::Num(n)) => ("", num_text(*n)),
                        Some(Value::Text(s)) => (" t=\"str\"", escape(s)),
                        Some(Value::Bool(b)) => (" t=\"b\"", format!("{}", *b as u8)),
                        Some(Value::Err(e)) => (" t=\"e\"", String::from(*e)),
                        _ => ("", String::new()),
                    };
                    let v = if v.is_empty() { String::new() } else { format!("<v>{}</v>", v) };
                    x += &format!("<c r=\"{}\"{}{}><f>{}</f>{}</c>", name, sattr, t, escape(f), v);
                }
            }
        }
        if cur_row.is_some() {
            x += "</row>";
        }
        x += "</sheetData></worksheet>";
        sheet_xml.push(x);
    }

    let mut styles = String::from("<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n<styleSheet xmlns=\"http://schemas.openxmlformats.org/spreadsheetml/2006/main\">");
    if !fmts.is_empty() {
        styles += &format!("<numFmts count=\"{}\">", fmts.len());
        for (i, f) in fmts.iter().enumerate() {
            styles += &format!("<numFmt numFmtId=\"{}\" formatCode=\"{}\"/>", 164 + i, escape(f));
        }
        styles += "</numFmts>";
    }
    styles += &format!("<fonts count=\"{}\">", fonts.len());
    for (b, i, c) in &fonts {
        styles += "<font>";
        if *b {
            styles += "<b/>";
        }
        if *i {
            styles += "<i/>";
        }
        styles += "<sz val=\"11\"/>";
        if let Some(c) = c {
            styles += &format!("<color rgb=\"FF{:06X}\"/>", c);
        }
        styles += "<name val=\"Calibri\"/><family val=\"2\"/></font>";
    }
    styles += "</fonts>";
    styles += &format!("<fills count=\"{}\"><fill><patternFill patternType=\"none\"/></fill><fill><patternFill patternType=\"gray125\"/></fill>", fills.len());
    for f in fills.iter().skip(2) {
        styles += &format!("<fill><patternFill patternType=\"solid\"><fgColor rgb=\"FF{:06X}\"/><bgColor indexed=\"64\"/></patternFill></fill>", f.unwrap_or(0xFFFFFF));
    }
    styles += "</fills><borders count=\"1\"><border><left/><right/><top/><bottom/><diagonal/></border></borders>";
    styles += "<cellStyleXfs count=\"1\"><xf numFmtId=\"0\" fontId=\"0\" fillId=\"0\" borderId=\"0\"/></cellStyleXfs>";
    styles += &format!("<cellXfs count=\"{}\">", xfs.len());
    for x in &xfs {
        let mut a = format!("<xf numFmtId=\"{}\" fontId=\"{}\" fillId=\"{}\" borderId=\"0\" xfId=\"0\"", x.fmt, x.font, x.fill);
        if x.fmt != 0 {
            a += " applyNumberFormat=\"1\"";
        }
        if x.font != 0 {
            a += " applyFont=\"1\"";
        }
        if x.fill != 0 {
            a += " applyFill=\"1\"";
        }
        match x.align {
            0 => a += "/>",
            al => {
                let h = match al {
                    -1 => "left",
                    1 => "center",
                    _ => "right",
                };
                a += &format!(" applyAlignment=\"1\"><alignment horizontal=\"{}\"/></xf>", h);
            }
        }
        styles += &a;
    }
    styles += "</cellXfs><cellStyles count=\"1\"><cellStyle name=\"Normal\" xfId=\"0\" builtinId=\"0\"/></cellStyles></styleSheet>";

    let mut workbook = String::from("<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n<workbook xmlns=\"http://schemas.openxmlformats.org/spreadsheetml/2006/main\" xmlns:r=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships\"><sheets>");
    let mut wrels = String::from("<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n<Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\">");
    let mut types = String::from("<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n<Types xmlns=\"http://schemas.openxmlformats.org/package/2006/content-types\"><Default Extension=\"rels\" ContentType=\"application/vnd.openxmlformats-package.relationships+xml\"/><Default Extension=\"xml\" ContentType=\"application/xml\"/><Override PartName=\"/xl/workbook.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml\"/><Override PartName=\"/xl/styles.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.spreadsheetml.styles+xml\"/>");
    for (i, sh) in wb.sheets.iter().enumerate() {
        workbook += &format!("<sheet name=\"{}\" sheetId=\"{}\" r:id=\"rId{}\"/>", escape(&sh.name), i + 1, i + 1);
        wrels += &format!("<Relationship Id=\"rId{0}\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet\" Target=\"worksheets/sheet{0}.xml\"/>", i + 1);
        types += &format!("<Override PartName=\"/xl/worksheets/sheet{}.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml\"/>", i + 1);
    }
    workbook += "</sheets><calcPr calcId=\"191029\" fullCalcOnLoad=\"1\"/></workbook>";
    let n = wb.sheets.len();
    wrels += &format!("<Relationship Id=\"rId{}\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/styles\" Target=\"styles.xml\"/></Relationships>", n + 1);
    types += "</Types>";
    let root_rels = "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n<Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\"><Relationship Id=\"rId1\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument\" Target=\"xl/workbook.xml\"/></Relationships>";

    let names: Vec<String> = (1..=n).map(|i| format!("xl/worksheets/sheet{}.xml", i)).collect();
    let mut files: Vec<(&str, &[u8])> = Vec::from([
        ("[Content_Types].xml", types.as_bytes()),
        ("_rels/.rels", root_rels.as_bytes()),
        ("xl/workbook.xml", workbook.as_bytes()),
        ("xl/_rels/workbook.xml.rels", wrels.as_bytes()),
        ("xl/styles.xml", styles.as_bytes()),
    ]);
    for (i, x) in sheet_xml.iter().enumerate() {
        files.push((names[i].as_str(), x.as_bytes()));
    }
    zip::write(&files)
}

/// A number as Excel stores it (shortest round-trip form).
fn num_text(n: f64) -> String {
    if n == libm::trunc(n) && libm::fabs(n) < 1e15 {
        format!("{}", n as i64)
    } else {
        format!("{}", n)
    }
}

// ---- CSV ------------------------------------------------------------------------------

pub fn read_csv(bytes: &[u8]) -> Workbook {
    let text = String::from_utf8_lossy(bytes);
    let text = text.strip_prefix('\u{feff}').unwrap_or(&text);
    // Semicolon-separated files (common in Europe) if the first line has more ; than ,
    let first = text.lines().next().unwrap_or("");
    let sep = if first.matches(';').count() > first.matches(',').count() { ';' } else { ',' };
    let mut wb = Workbook::new();
    let (mut r, mut c) = (0u32, 0u32);
    let mut field = String::new();
    let mut quoted = false;
    let mut chars = text.chars().peekable();
    let mut was_quoted = false;
    let put = |wb: &mut Workbook, r: u32, c: u32, f: &str, was_quoted: bool| {
        if f.is_empty() {
            return;
        }
        if was_quoted {
            wb.sheets[0].cells.insert((r, c), Cell { content: Content::Text(String::from(f)), style: Style::default() });
        } else {
            wb.set_input(0, r, c, f.trim_start_matches('='));
        }
    };
    while let Some(ch) = chars.next() {
        if quoted {
            if ch == '"' {
                if chars.peek() == Some(&'"') {
                    field.push('"');
                    chars.next();
                } else {
                    quoted = false;
                }
            } else {
                field.push(ch);
            }
            continue;
        }
        match ch {
            '"' if field.is_empty() => {
                quoted = true;
                was_quoted = true;
            }
            c2 if c2 == sep => {
                put(&mut wb, r, c, &field, was_quoted);
                field.clear();
                was_quoted = false;
                c += 1;
            }
            '\r' => {}
            '\n' => {
                put(&mut wb, r, c, &field, was_quoted);
                field.clear();
                was_quoted = false;
                r += 1;
                c = 0;
            }
            other => field.push(other),
        }
    }
    put(&mut wb, r, c, &field, was_quoted);
    wb.recalc();
    wb
}

/// The first sheet's shown values as CSV.
pub fn write_csv(wb: &Workbook) -> Vec<u8> {
    let mut out = String::new();
    if let Some((rows, cols)) = wb.sheets[0].extent() {
        for r in 0..=rows {
            for c in 0..=cols {
                if c > 0 {
                    out.push(',');
                }
                let v = wb.display(0, r, c);
                if v.contains(',') || v.contains('"') || v.contains('\n') {
                    out.push('"');
                    out.push_str(&v.replace('"', "\"\""));
                    out.push('"');
                } else {
                    out.push_str(&v);
                }
            }
            out.push_str("\r\n");
        }
    }
    out.into_bytes()
}
