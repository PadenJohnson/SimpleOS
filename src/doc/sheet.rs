//! The spreadsheet model: workbooks, sheets, cells, styles, values and number formats.

use alloc::collections::BTreeMap;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use super::formula;

pub const MAX_ROWS: u32 = 1_048_576;
pub const MAX_COLS: u32 = 16_384;
/// Default column width in Excel "characters".
pub const DEFAULT_WIDTH: f32 = 8.43;

#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    Empty,
    Num(f64),
    Text(String),
    Bool(bool),
    Err(&'static str),
}

impl Value {
    pub fn as_num(&self) -> Result<f64, &'static str> {
        match self {
            Value::Num(n) => Ok(*n),
            Value::Bool(b) => Ok(if *b { 1.0 } else { 0.0 }),
            Value::Empty => Ok(0.0),
            Value::Text(t) => parse_number(t).map(|(n, _)| n).ok_or("#VALUE!"),
            Value::Err(e) => Err(e),
        }
    }
    pub fn as_text(&self) -> String {
        match self {
            Value::Num(n) => general(*n),
            Value::Text(t) => t.clone(),
            Value::Bool(b) => String::from(if *b { "TRUE" } else { "FALSE" }),
            Value::Empty => String::new(),
            Value::Err(e) => String::from(*e),
        }
    }
}

/// What the user typed (or the file stored).
#[derive(Clone, Debug, PartialEq)]
pub enum Content {
    Num(f64),
    Text(String),
    Bool(bool),
    /// Formula text without the leading "=", and the value the file last saved for it
    /// (shown if Solon can't work the formula out itself).
    Formula(String, Option<Value>),
}

#[derive(Clone, Debug, PartialEq, Default)]
pub enum NumFmt {
    #[default]
    General,
    /// 0, 0.00, #,##0.00 ...
    Number { decimals: u8, thousands: bool },
    Money { symbol: char, decimals: u8 },
    Percent { decimals: u8 },
    Date,
    /// A format code from a file Solon only approximates (kept when saving).
    Custom(String),
}

impl NumFmt {
    pub fn label(&self) -> &'static str {
        match self {
            NumFmt::General => "General",
            NumFmt::Number { .. } => "Number",
            NumFmt::Money { .. } => "Money",
            NumFmt::Percent { .. } => "Percent",
            NumFmt::Date => "Date",
            NumFmt::Custom(_) => "Custom",
        }
    }

    /// The Excel format code.
    pub fn code(&self) -> String {
        let dec = |d: u8| if d == 0 { String::new() } else { format!(".{}", "0".repeat(d as usize)) };
        match self {
            NumFmt::General => String::from("General"),
            NumFmt::Number { decimals, thousands } => format!("{}{}", if *thousands { "#,##0" } else { "0" }, dec(*decimals)),
            NumFmt::Money { symbol, decimals } => format!("\"{}\"#,##0{}", symbol, dec(*decimals)),
            NumFmt::Percent { decimals } => format!("0{}%", dec(*decimals)),
            NumFmt::Date => String::from("m/d/yyyy"),
            NumFmt::Custom(c) => c.clone(),
        }
    }

    /// Understand a format code from a file.
    pub fn from_code(code: &str) -> NumFmt {
        if code.eq_ignore_ascii_case("general") || code.is_empty() {
            return NumFmt::General;
        }
        // Only the first section (positive numbers) matters for display.
        let first = code.split(';').next().unwrap_or(code);
        // Strip quoted text, [colors]/[$€-x] and escapes to look at the bare pattern.
        let mut bare = String::new();
        let mut symbol = None;
        let mut chars = first.chars().peekable();
        while let Some(c) = chars.next() {
            match c {
                '"' => {
                    for q in chars.by_ref() {
                        if q == '"' {
                            break;
                        }
                        if "$€£¥".contains(q) {
                            symbol = Some(q);
                        }
                    }
                }
                '[' => {
                    let mut inner = String::new();
                    for q in chars.by_ref() {
                        if q == ']' {
                            break;
                        }
                        inner.push(q);
                    }
                    if let Some(s) = inner.strip_prefix('$') {
                        if let Some(ch) = s.chars().next().filter(|c| *c != '-') {
                            symbol = Some(ch);
                        }
                    }
                }
                '\\' => {
                    if let Some(q) = chars.next() {
                        if "$€£¥".contains(q) {
                            symbol = Some(q);
                        }
                    }
                }
                '$' | '€' | '£' | '¥' => symbol = Some(c),
                _ => bare.push(c),
            }
        }
        let lower = bare.to_ascii_lowercase();
        let decimals = lower.split('.').nth(1).map(|d| d.chars().take_while(|c| *c == '0').count() as u8).unwrap_or(0);
        if lower.contains('y') || lower.contains('d') || (lower.contains('m') && !lower.contains('0') && !lower.contains('#')) {
            return if code == "m/d/yyyy" { NumFmt::Date } else { NumFmt::Custom(String::from(code)) };
        }
        let fmt = if lower.contains('%') {
            NumFmt::Percent { decimals }
        } else if let Some(s) = symbol {
            NumFmt::Money { symbol: s, decimals }
        } else if lower.contains('0') || lower.contains('#') {
            NumFmt::Number { decimals, thousands: lower.contains(',') }
        } else {
            return NumFmt::Custom(String::from(code));
        };
        // Keep the file's own code if ours would differ (so saving doesn't change it).
        if fmt.code() == code {
            fmt
        } else {
            NumFmt::Custom(String::from(code))
        }
    }

    /// How a custom code displays: the closest built-in format.
    fn effective(&self) -> NumFmt {
        match self {
            NumFmt::Custom(code) => {
                let lower = code.to_ascii_lowercase();
                let first = lower.split(';').next().unwrap_or("");
                let no_quotes: String = first.split('"').step_by(2).collect();
                if no_quotes.contains('y') || no_quotes.contains('d') || (no_quotes.contains('m') && !no_quotes.contains('0')) {
                    return NumFmt::Date;
                }
                let decimals = no_quotes.split('.').nth(1).map(|d| d.chars().take_while(|c| *c == '0').count() as u8).unwrap_or(0);
                let symbol = code.chars().find(|c| "$€£¥".contains(*c));
                if no_quotes.contains('%') {
                    NumFmt::Percent { decimals }
                } else if let Some(s) = symbol {
                    NumFmt::Money { symbol: s, decimals }
                } else if no_quotes.contains('0') || no_quotes.contains('#') {
                    NumFmt::Number { decimals, thousands: no_quotes.contains(',') }
                } else {
                    NumFmt::General
                }
            }
            other => other.clone(),
        }
    }

    pub fn is_date(&self) -> bool {
        self.effective() == NumFmt::Date
    }

    pub fn show(&self, n: f64) -> String {
        match self.effective() {
            NumFmt::General | NumFmt::Custom(_) => general(n),
            NumFmt::Number { decimals, thousands } => fixed(n, decimals, thousands),
            NumFmt::Money { symbol, decimals } => {
                let s = fixed(libm::fabs(n), decimals, true);
                if n < 0.0 {
                    format!("-{}{}", symbol, s)
                } else {
                    format!("{}{}", symbol, s)
                }
            }
            NumFmt::Percent { decimals } => format!("{}%", fixed(n * 100.0, decimals, false)),
            NumFmt::Date => date_text(n),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Default)]
pub struct Style {
    pub bold: bool,
    pub italic: bool,
    /// Text colour (0xRRGGBB), None = black.
    pub color: Option<u32>,
    /// Fill colour, None = no fill.
    pub fill: Option<u32>,
    pub fmt: NumFmt,
    /// -1 left, 0 automatic, 1 centre, 2 right.
    pub align: i8,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Cell {
    pub content: Content,
    pub style: Style,
}

#[derive(Clone, Debug, Default)]
pub struct Sheet {
    pub name: String,
    /// (row, col) from 0.
    pub cells: BTreeMap<(u32, u32), Cell>,
    /// Width in characters for columns that aren't the default.
    pub widths: BTreeMap<u32, f32>,
    /// Styles of empty cells that were formatted (kept so a later value shows formatted).
    pub styles: BTreeMap<(u32, u32), Style>,
}

impl Sheet {
    pub fn new(name: &str) -> Sheet {
        Sheet { name: String::from(name), ..Default::default() }
    }
    pub fn width(&self, col: u32) -> f32 {
        self.widths.get(&col).copied().unwrap_or(DEFAULT_WIDTH)
    }
    pub fn style(&self, r: u32, c: u32) -> Style {
        self.cells.get(&(r, c)).map(|x| x.style.clone()).or_else(|| self.styles.get(&(r, c)).cloned()).unwrap_or_default()
    }
    /// (last row, last col) used, or None if empty.
    pub fn extent(&self) -> Option<(u32, u32)> {
        let mut out: Option<(u32, u32)> = None;
        for &(r, c) in self.cells.keys() {
            out = Some(match out {
                None => (r, c),
                Some((mr, mc)) => (mr.max(r), mc.max(c)),
            });
        }
        out
    }
}

#[derive(Clone, Debug)]
pub struct Workbook {
    pub sheets: Vec<Sheet>,
    /// Computed values of formula cells: (sheet, row, col).
    pub values: BTreeMap<(usize, u32, u32), Value>,
}

impl Workbook {
    pub fn new() -> Workbook {
        Workbook { sheets: Vec::from([Sheet::new("Sheet1")]), values: BTreeMap::new() }
    }

    /// Work out every formula again.
    pub fn recalc(&mut self) {
        self.values = formula::evaluate_all(self);
    }

    pub fn value(&self, s: usize, r: u32, c: u32) -> Value {
        let Some(cell) = self.sheets.get(s).and_then(|sh| sh.cells.get(&(r, c))) else { return Value::Empty };
        match &cell.content {
            Content::Num(n) => Value::Num(*n),
            Content::Text(t) => Value::Text(t.clone()),
            Content::Bool(b) => Value::Bool(*b),
            Content::Formula(..) => self.values.get(&(s, r, c)).cloned().unwrap_or(Value::Empty),
        }
    }

    /// What a cell shows.
    pub fn display(&self, s: usize, r: u32, c: u32) -> String {
        let style = self.sheets[s].style(r, c);
        match self.value(s, r, c) {
            Value::Num(n) => style.fmt.show(n),
            v => v.as_text(),
        }
    }

    /// What the edit box shows for a cell.
    pub fn edit_text(&self, s: usize, r: u32, c: u32) -> String {
        let sh = &self.sheets[s];
        match sh.cells.get(&(r, c)).map(|x| &x.content) {
            None => String::new(),
            Some(Content::Formula(f, _)) => format!("={}", f),
            Some(Content::Num(n)) => {
                let fmt = sh.style(r, c).fmt;
                match fmt.effective() {
                    NumFmt::Date => date_text(*n),
                    NumFmt::Percent { .. } => format!("{}%", general(*n * 100.0)),
                    _ => general(*n),
                }
            }
            Some(Content::Text(t)) => t.clone(),
            Some(Content::Bool(b)) => String::from(if *b { "TRUE" } else { "FALSE" }),
        }
    }

    /// Put what the user typed into a cell (empty clears it, keeping its formatting).
    pub fn set_input(&mut self, s: usize, r: u32, c: u32, input: &str) {
        let sh = &mut self.sheets[s];
        let mut style = sh.style(r, c);
        let content = if input.is_empty() {
            None
        } else if let Some(f) = input.strip_prefix('=').filter(|f| !f.is_empty()) {
            Some(Content::Formula(formula::normalize(f.trim()), None))
        } else if input.eq_ignore_ascii_case("true") || input.eq_ignore_ascii_case("false") {
            Some(Content::Bool(input.eq_ignore_ascii_case("true")))
        } else if let Some(n) = parse_date(input) {
            if !style.fmt.is_date() {
                style.fmt = NumFmt::Date;
            }
            Some(Content::Num(n))
        } else if let Some((n, fmt)) = parse_number(input) {
            if style.fmt == NumFmt::General && fmt != NumFmt::General {
                style.fmt = fmt;
            }
            Some(Content::Num(n))
        } else {
            Some(Content::Text(String::from(input)))
        };
        match content {
            Some(content) => {
                sh.styles.remove(&(r, c));
                sh.cells.insert((r, c), Cell { content, style });
            }
            None => {
                sh.cells.remove(&(r, c));
                if style != Style::default() {
                    sh.styles.insert((r, c), style);
                }
            }
        }
    }

    /// Change the style of every cell in a range.
    pub fn restyle(&mut self, s: usize, r0: u32, c0: u32, r1: u32, c1: u32, f: &dyn Fn(&mut Style)) {
        let sh = &mut self.sheets[s];
        // Large selections (a whole column) only touch cells that exist.
        let area = (r1 - r0 + 1) as u64 * (c1 - c0 + 1) as u64;
        if area > 20_000 {
            for ((r, c), cell) in sh.cells.iter_mut() {
                if (r0..=r1).contains(r) && (c0..=c1).contains(c) {
                    f(&mut cell.style);
                }
            }
            return;
        }
        for r in r0..=r1 {
            for c in c0..=c1 {
                if let Some(cell) = sh.cells.get_mut(&(r, c)) {
                    f(&mut cell.style);
                } else {
                    let mut st = sh.styles.get(&(r, c)).cloned().unwrap_or_default();
                    f(&mut st);
                    if st == Style::default() {
                        sh.styles.remove(&(r, c));
                    } else {
                        sh.styles.insert((r, c), st);
                    }
                }
            }
        }
    }

    /// A sheet name not used yet ("Sheet2", ...).
    pub fn new_sheet_name(&self) -> String {
        (1..).map(|n| format!("Sheet{}", n)).find(|n| !self.sheets.iter().any(|s| s.name.eq_ignore_ascii_case(n))).unwrap()
    }

    /// True if saving as CSV would lose something (formatting, formulas, more sheets).
    pub fn needs_xlsx(&self) -> bool {
        self.sheets.len() > 1
            || self.sheets.iter().any(|s| {
                !s.widths.is_empty() || !s.styles.is_empty() || s.cells.values().any(|c| c.style != Style::default() || matches!(c.content, Content::Formula(..)))
            })
    }
}

// ---- cell names ----------------------------------------------------------------------

/// "A", "B", ... "Z", "AA" ...
pub fn col_name(c: u32) -> String {
    let mut n = c + 1;
    let mut s = Vec::new();
    while n > 0 {
        let r = (n - 1) % 26;
        s.push(b'A' + r as u8);
        n = (n - 1) / 26;
    }
    s.reverse();
    String::from_utf8(s).unwrap()
}

pub fn cell_name(r: u32, c: u32) -> String {
    format!("{}{}", col_name(c), r + 1)
}

/// "B12" -> (row 11, col 1). Accepts "$" markers.
pub fn parse_cell(s: &str) -> Option<(u32, u32)> {
    let s = s.replace('$', "");
    let split = s.find(|c: char| c.is_ascii_digit())?;
    let (letters, digits) = s.split_at(split);
    if letters.is_empty() || !letters.chars().all(|c| c.is_ascii_alphabetic()) || !digits.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    let mut col: u32 = 0;
    for ch in letters.chars() {
        col = col.checked_mul(26)? + (ch.to_ascii_uppercase() as u32 - 'A' as u32 + 1);
    }
    let row: u32 = digits.parse().ok()?;
    if row == 0 || col == 0 || row > MAX_ROWS || col > MAX_COLS {
        return None;
    }
    Some((row - 1, col - 1))
}

// ---- numbers -------------------------------------------------------------------------

/// Excel's "General" display: up to about 11 significant digits.
pub fn general(n: f64) -> String {
    if n.is_nan() || n.is_infinite() {
        return String::from("#NUM!");
    }
    if n == 0.0 {
        return String::from("0");
    }
    let a = libm::fabs(n);
    if a >= 1e11 || a < 1e-9 {
        // Scientific: 1.23457E+12
        let exp = libm::floor(libm::log10(a)) as i32;
        let mant = n / libm::pow(10.0, exp as f64);
        let m = trim_zeros(format!("{:.5}", mant));
        return format!("{}E{}{:02}", m, if exp < 0 { '-' } else { '+' }, exp.abs());
    }
    let int_digits = if a >= 1.0 { libm::floor(libm::log10(a)) as i32 + 1 } else { 1 };
    let decimals = (10 - int_digits).clamp(0, 10) as usize;
    trim_zeros(format!("{:.*}", decimals, n))
}

fn trim_zeros(s: String) -> String {
    if !s.contains('.') {
        return s;
    }
    let t = s.trim_end_matches('0').trim_end_matches('.');
    if t == "-0" {
        String::from("0")
    } else {
        String::from(t)
    }
}

/// Fixed decimals, optionally with thousands commas.
pub fn fixed(n: f64, decimals: u8, thousands: bool) -> String {
    let s = format!("{:.*}", decimals as usize, n);
    if !thousands {
        return s;
    }
    let (sign, rest) = match s.strip_prefix('-') {
        Some(r) => ("-", r),
        None => ("", s.as_str()),
    };
    let (int, frac) = match rest.find('.') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, ""),
    };
    let mut out = String::new();
    for (i, ch) in int.chars().enumerate() {
        if i > 0 && (int.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(ch);
    }
    format!("{}{}{}", sign, out, frac)
}

/// "1,234.5" "$12.50" "-3" "45%" "1e3" -> number and the format it suggests.
pub fn parse_number(s: &str) -> Option<(f64, NumFmt)> {
    let t = s.trim();
    if t.is_empty() {
        return None;
    }
    let (neg, t) = match t.strip_prefix('-') {
        Some(r) => (true, r.trim_start()),
        None => (false, t),
    };
    let mut fmt = NumFmt::General;
    let mut body = String::from(t);
    if let Some(sym) = body.chars().next().filter(|c| "$€£¥".contains(*c)) {
        body = String::from(&body[sym.len_utf8()..]);
        fmt = NumFmt::Money { symbol: sym, decimals: 2 };
    }
    let pct = body.ends_with('%');
    if pct {
        body.pop();
    }
    let had_commas = body.contains(',');
    if had_commas {
        // Commas only as thousands separators.
        let int = body.split('.').next().unwrap_or("");
        let groups: Vec<&str> = int.split(',').collect();
        if groups[0].is_empty() || groups[0].len() > 3 || groups[1..].iter().any(|g| g.len() != 3) {
            return None;
        }
        body = body.replace(',', "");
    }
    if body.is_empty() || !body.chars().all(|c| c.is_ascii_digit() || c == '.' || c == 'e' || c == 'E' || c == '+' || c == '-') {
        return None;
    }
    if !body.chars().next().is_some_and(|c| c.is_ascii_digit() || c == '.') {
        return None;
    }
    let mut n: f64 = body.parse().ok()?;
    if neg {
        n = -n;
    }
    if pct {
        n /= 100.0;
        let decimals = body.split('.').nth(1).map(|d| d.len() as u8).unwrap_or(0);
        fmt = NumFmt::Percent { decimals };
    } else if had_commas && fmt == NumFmt::General {
        let decimals = body.split('.').nth(1).map(|d| d.len().min(4) as u8).unwrap_or(0);
        fmt = NumFmt::Number { decimals, thousands: true };
    }
    Some((n, fmt))
}

// ---- dates (Excel serial numbers, 1900 system) -------------------------------------------

fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// Days from 1970-01-01 to Excel's day 0 (1899-12-30, which makes serial 61 = 1900-03-01).
const EXCEL_EPOCH: i64 = -25569;

pub fn date_serial(y: i64, m: i64, d: i64) -> f64 {
    (days_from_civil(y, m, d) - EXCEL_EPOCH) as f64
}

pub fn date_parts(serial: f64) -> (i64, i64, i64) {
    civil_from_days(libm::floor(serial) as i64 + EXCEL_EPOCH)
}

pub fn date_text(serial: f64) -> String {
    if !(1.0..2_958_466.0).contains(&serial) {
        return general(serial);
    }
    let (y, m, d) = date_parts(serial);
    format!("{}/{}/{}", m, d, y)
}

/// "11/20/2026" or "2026-11-20" -> serial.
pub fn parse_date(s: &str) -> Option<f64> {
    let t = s.trim();
    let parts: Vec<&str> = if t.contains('/') { t.split('/').collect() } else if t.matches('-').count() == 2 && !t.starts_with('-') { t.split('-').collect() } else { return None };
    if parts.len() != 3 || parts.iter().any(|p| p.is_empty() || !p.chars().all(|c| c.is_ascii_digit())) {
        return None;
    }
    let n: Vec<i64> = parts.iter().map(|p| p.parse().unwrap_or(0)).collect();
    let (y, m, d) = if parts[0].len() == 4 { (n[0], n[1], n[2]) } else { (if n[2] < 100 { 2000 + n[2] } else { n[2] }, n[0], n[1]) };
    if !(1..=12).contains(&m) || !(1..=31).contains(&d) || !(1900..=9999).contains(&y) {
        return None;
    }
    let serial = date_serial(y, m, d);
    // Reject 2/30 etc.
    if date_parts(serial) != (y, m, d) {
        return None;
    }
    Some(serial)
}

pub fn today_serial() -> f64 {
    let (y, m, d) = crate::rtc::date();
    date_serial(y as i64, m as i64, d as i64)
}
