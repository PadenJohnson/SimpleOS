//! Formulas: a parser and evaluator for the common Excel subset.
//!
//! Operators: + - * / ^ & % = <> < > <= >=, references (A1, $A$1, Sheet2!A1,
//! 'My sheet'!A1:B3), and functions SUM AVERAGE MIN MAX COUNT COUNTA IF IFERROR ROUND
//! ROUNDUP ROUNDDOWN ABS AND OR NOT CONCATENATE CONCAT LEN UPPER LOWER TRIM LEFT RIGHT MID
//! TODAY SUMIF COUNTIF PRODUCT INT MOD SQRT POWER. Anything else shows the value the file
//! last saved, or #NAME?.

use alloc::boxed::Box;
use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec::Vec;

use super::sheet::{general, parse_cell, today_serial, Content, Value, Workbook};

#[derive(Clone, Debug, PartialEq)]
enum Tok {
    Num(f64),
    Str(String),
    /// A cell reference, maybe with a sheet: (sheet, row, col)
    Ref(Option<String>, u32, u32),
    Ident(String),
    Op(&'static str),
    LParen,
    RParen,
    Comma,
    Colon,
}

#[derive(Clone, Debug)]
enum Expr {
    Num(f64),
    Str(String),
    Bool(bool),
    Ref(Option<String>, u32, u32),
    Range(Option<String>, (u32, u32), (u32, u32)),
    Neg(Box<Expr>),
    Pct(Box<Expr>),
    Bin(&'static str, Box<Expr>, Box<Expr>),
    Call(String, Vec<Expr>),
}

const ERR_VALUE: &str = "#VALUE!";
const ERR_DIV: &str = "#DIV/0!";
const ERR_REF: &str = "#REF!";
const ERR_NAME: &str = "#NAME?";
const ERR_CYCLE: &str = "#CYCLE!";
const ERR_NUM: &str = "#NUM!";

fn tokenize(s: &str) -> Result<Vec<Tok>, &'static str> {
    let b: Vec<char> = s.chars().collect();
    let mut i = 0;
    let mut out = Vec::new();
    while i < b.len() {
        let c = b[i];
        if c.is_whitespace() {
            i += 1;
            continue;
        }
        if c.is_ascii_digit() || (c == '.' && b.get(i + 1).is_some_and(|d| d.is_ascii_digit())) {
            let st = i;
            while i < b.len() && (b[i].is_ascii_digit() || b[i] == '.') {
                i += 1;
            }
            if i < b.len() && (b[i] == 'e' || b[i] == 'E') && b.get(i + 1).is_some_and(|d| d.is_ascii_digit() || *d == '-' || *d == '+') {
                i += 2;
                while i < b.len() && b[i].is_ascii_digit() {
                    i += 1;
                }
            }
            let t: String = b[st..i].iter().collect();
            // "1:3" style row ranges aren't supported; plain numbers only.
            out.push(Tok::Num(t.parse().map_err(|_| ERR_VALUE)?));
            continue;
        }
        if c == '"' {
            let mut t = String::new();
            i += 1;
            loop {
                match b.get(i) {
                    None => return Err(ERR_VALUE),
                    Some('"') if b.get(i + 1) == Some(&'"') => {
                        t.push('"');
                        i += 2;
                    }
                    Some('"') => {
                        i += 1;
                        break;
                    }
                    Some(&ch) => {
                        t.push(ch);
                        i += 1;
                    }
                }
            }
            out.push(Tok::Str(t));
            continue;
        }
        if c == '\'' {
            // 'Sheet name'!A1
            let mut name = String::new();
            i += 1;
            loop {
                match b.get(i) {
                    None => return Err(ERR_REF),
                    Some('\'') if b.get(i + 1) == Some(&'\'') => {
                        name.push('\'');
                        i += 2;
                    }
                    Some('\'') => {
                        i += 1;
                        break;
                    }
                    Some(&ch) => {
                        name.push(ch);
                        i += 1;
                    }
                }
            }
            if b.get(i) != Some(&'!') {
                return Err(ERR_REF);
            }
            i += 1;
            let st = i;
            while i < b.len() && (b[i].is_ascii_alphanumeric() || b[i] == '$') {
                i += 1;
            }
            let r: String = b[st..i].iter().collect();
            let (row, col) = parse_cell(&r).ok_or(ERR_REF)?;
            out.push(Tok::Ref(Some(name), row, col));
            continue;
        }
        if c.is_ascii_alphabetic() || c == '$' || c == '_' {
            let st = i;
            while i < b.len() && (b[i].is_ascii_alphanumeric() || b[i] == '$' || b[i] == '_' || b[i] == '.') {
                i += 1;
            }
            let word: String = b[st..i].iter().collect();
            if b.get(i) == Some(&'!') {
                // Sheet1!A1
                i += 1;
                let st = i;
                while i < b.len() && (b[i].is_ascii_alphanumeric() || b[i] == '$') {
                    i += 1;
                }
                let r: String = b[st..i].iter().collect();
                let (row, col) = parse_cell(&r).ok_or(ERR_REF)?;
                out.push(Tok::Ref(Some(word), row, col));
            } else if let (Some((row, col)), false) = (parse_cell(&word), b.get(i) == Some(&'(')) {
                out.push(Tok::Ref(None, row, col));
            } else {
                out.push(Tok::Ident(word.to_ascii_uppercase()));
            }
            continue;
        }
        let two: String = b[i..(i + 2).min(b.len())].iter().collect();
        let op2 = match two.as_str() {
            "<=" => Some("<="),
            ">=" => Some(">="),
            "<>" => Some("<>"),
            _ => None,
        };
        if let Some(op) = op2 {
            out.push(Tok::Op(op));
            i += 2;
            continue;
        }
        out.push(match c {
            '+' => Tok::Op("+"),
            '-' => Tok::Op("-"),
            '*' => Tok::Op("*"),
            '/' => Tok::Op("/"),
            '^' => Tok::Op("^"),
            '&' => Tok::Op("&"),
            '%' => Tok::Op("%"),
            '=' => Tok::Op("="),
            '<' => Tok::Op("<"),
            '>' => Tok::Op(">"),
            '(' => Tok::LParen,
            ')' => Tok::RParen,
            ',' | ';' => Tok::Comma,
            ':' => Tok::Colon,
            _ => return Err(ERR_NAME),
        });
        i += 1;
    }
    Ok(out)
}

struct Parser {
    t: Vec<Tok>,
    p: usize,
}

impl Parser {
    fn peek(&self) -> Option<&Tok> {
        self.t.get(self.p)
    }
    fn next(&mut self) -> Option<Tok> {
        let t = self.t.get(self.p).cloned();
        self.p += 1;
        t
    }
    fn op(&self) -> Option<&'static str> {
        match self.peek() {
            Some(Tok::Op(o)) => Some(o),
            _ => None,
        }
    }

    fn compare(&mut self) -> Result<Expr, &'static str> {
        let mut l = self.concat()?;
        while let Some(o) = self.op().filter(|o| matches!(*o, "=" | "<>" | "<" | ">" | "<=" | ">=")) {
            self.p += 1;
            let r = self.concat()?;
            l = Expr::Bin(o, Box::new(l), Box::new(r));
        }
        Ok(l)
    }
    fn concat(&mut self) -> Result<Expr, &'static str> {
        let mut l = self.add()?;
        while self.op() == Some("&") {
            self.p += 1;
            let r = self.add()?;
            l = Expr::Bin("&", Box::new(l), Box::new(r));
        }
        Ok(l)
    }
    fn add(&mut self) -> Result<Expr, &'static str> {
        let mut l = self.mul()?;
        while let Some(o) = self.op().filter(|o| matches!(*o, "+" | "-")) {
            self.p += 1;
            let r = self.mul()?;
            l = Expr::Bin(o, Box::new(l), Box::new(r));
        }
        Ok(l)
    }
    fn mul(&mut self) -> Result<Expr, &'static str> {
        let mut l = self.pow()?;
        while let Some(o) = self.op().filter(|o| matches!(*o, "*" | "/")) {
            self.p += 1;
            let r = self.pow()?;
            l = Expr::Bin(o, Box::new(l), Box::new(r));
        }
        Ok(l)
    }
    fn pow(&mut self) -> Result<Expr, &'static str> {
        let mut l = self.unary()?;
        while self.op() == Some("^") {
            self.p += 1;
            let r = self.unary()?;
            l = Expr::Bin("^", Box::new(l), Box::new(r));
        }
        Ok(l)
    }
    fn unary(&mut self) -> Result<Expr, &'static str> {
        match self.op() {
            Some("-") => {
                self.p += 1;
                Ok(Expr::Neg(Box::new(self.unary()?)))
            }
            Some("+") => {
                self.p += 1;
                self.unary()
            }
            _ => self.postfix(),
        }
    }
    fn postfix(&mut self) -> Result<Expr, &'static str> {
        let mut e = self.primary()?;
        while self.op() == Some("%") {
            self.p += 1;
            e = Expr::Pct(Box::new(e));
        }
        Ok(e)
    }
    fn primary(&mut self) -> Result<Expr, &'static str> {
        match self.next().ok_or(ERR_VALUE)? {
            Tok::Num(n) => Ok(Expr::Num(n)),
            Tok::Str(s) => Ok(Expr::Str(s)),
            Tok::Ref(sheet, r, c) => {
                if self.peek() == Some(&Tok::Colon) {
                    self.p += 1;
                    match self.next() {
                        Some(Tok::Ref(_, r2, c2)) => Ok(Expr::Range(sheet, (r.min(r2), c.min(c2)), (r.max(r2), c.max(c2)))),
                        _ => Err(ERR_REF),
                    }
                } else {
                    Ok(Expr::Ref(sheet, r, c))
                }
            }
            Tok::Ident(name) => {
                if self.peek() == Some(&Tok::LParen) {
                    self.p += 1;
                    let mut args = Vec::new();
                    if self.peek() == Some(&Tok::RParen) {
                        self.p += 1;
                        return Ok(Expr::Call(name, args));
                    }
                    loop {
                        args.push(self.compare()?);
                        match self.next() {
                            Some(Tok::Comma) => continue,
                            Some(Tok::RParen) => break,
                            _ => return Err(ERR_VALUE),
                        }
                    }
                    Ok(Expr::Call(name, args))
                } else if name == "TRUE" {
                    Ok(Expr::Bool(true))
                } else if name == "FALSE" {
                    Ok(Expr::Bool(false))
                } else {
                    Err(ERR_NAME)
                }
            }
            Tok::LParen => {
                let e = self.compare()?;
                if self.next() != Some(Tok::RParen) {
                    return Err(ERR_VALUE);
                }
                Ok(e)
            }
            _ => Err(ERR_VALUE),
        }
    }
}

fn parse(f: &str) -> Result<Expr, &'static str> {
    let mut p = Parser { t: tokenize(f)?, p: 0 };
    let e = p.compare()?;
    if p.p != p.t.len() {
        return Err(ERR_VALUE);
    }
    Ok(e)
}

/// The names of functions Solon can calculate.
const KNOWN: [&str; 33] = [
    "SUM", "AVERAGE", "MIN", "MAX", "COUNT", "COUNTA", "IF", "IFERROR", "ROUND", "ROUNDUP", "ROUNDDOWN", "ABS", "AND", "OR", "NOT", "CONCATENATE", "CONCAT", "LEN", "UPPER",
    "LOWER", "TRIM", "LEFT", "RIGHT", "MID", "TODAY", "SUMIF", "COUNTIF", "PRODUCT", "INT", "MOD", "SQRT", "POWER", "NOW",
];

fn uses_unknown(e: &Expr) -> bool {
    match e {
        Expr::Call(n, args) => !KNOWN.contains(&n.as_str()) || args.iter().any(uses_unknown),
        Expr::Neg(x) | Expr::Pct(x) => uses_unknown(x),
        Expr::Bin(_, a, b) => uses_unknown(a) || uses_unknown(b),
        _ => false,
    }
}

struct Eval<'a> {
    wb: &'a Workbook,
    done: BTreeMap<(usize, u32, u32), Value>,
    busy: Vec<(usize, u32, u32)>,
}

/// One argument: a single value, or every value of a range (empty cells left out).
enum Arg {
    One(Value),
    Many(Vec<Value>),
}

impl<'a> Eval<'a> {
    fn sheet_index(&self, cur: usize, name: &Option<String>) -> Option<usize> {
        match name {
            None => Some(cur),
            Some(n) => self.wb.sheets.iter().position(|s| s.name.eq_ignore_ascii_case(n)),
        }
    }

    fn cell(&mut self, s: usize, r: u32, c: u32) -> Value {
        let Some(cell) = self.wb.sheets.get(s).and_then(|sh| sh.cells.get(&(r, c))) else { return Value::Empty };
        match &cell.content {
            Content::Num(n) => Value::Num(*n),
            Content::Text(t) => Value::Text(t.clone()),
            Content::Bool(b) => Value::Bool(*b),
            Content::Formula(f, cached) => {
                if let Some(v) = self.done.get(&(s, r, c)) {
                    return v.clone();
                }
                if self.busy.contains(&(s, r, c)) {
                    return Value::Err(ERR_CYCLE);
                }
                self.busy.push((s, r, c));
                let v = match parse(f) {
                    Ok(e) if uses_unknown(&e) => cached.clone().unwrap_or(Value::Err(ERR_NAME)),
                    Ok(e) => self.eval(s, &e),
                    Err(err) => cached.clone().unwrap_or(Value::Err(err)),
                };
                self.busy.pop();
                // A cell in a cycle isn't final; everything else is remembered.
                if v != Value::Err(ERR_CYCLE) || self.busy.is_empty() {
                    self.done.insert((s, r, c), v.clone());
                }
                v
            }
        }
    }

    fn arg(&mut self, s: usize, e: &Expr) -> Arg {
        match e {
            Expr::Range(name, (r0, c0), (r1, c1)) => {
                let Some(si) = self.sheet_index(s, name) else { return Arg::One(Value::Err(ERR_REF)) };
                let mut v = Vec::new();
                // Only cells that exist (ranges like A:A over a million rows stay fast).
                let keys: Vec<(u32, u32)> = self.wb.sheets[si].cells.range((*r0, 0)..=(*r1, u32::MAX)).map(|(k, _)| *k).filter(|(_, c)| (*c0..=*c1).contains(c)).collect();
                for (r, c) in keys {
                    v.push(self.cell(si, r, c));
                }
                Arg::Many(v)
            }
            other => Arg::One(self.eval(s, other)),
        }
    }

    fn nums(&mut self, s: usize, args: &[Expr]) -> Result<Vec<f64>, &'static str> {
        let mut out = Vec::new();
        for a in args {
            match self.arg(s, a) {
                Arg::One(Value::Err(e)) => return Err(e),
                Arg::One(v) => out.push(v.as_num()?),
                Arg::Many(vs) => {
                    for v in vs {
                        match v {
                            Value::Num(n) => out.push(n),
                            Value::Err(e) => return Err(e),
                            _ => {} // text and blanks in ranges are skipped, like Excel
                        }
                    }
                }
            }
        }
        Ok(out)
    }

    fn eval(&mut self, s: usize, e: &Expr) -> Value {
        match e {
            Expr::Num(n) => Value::Num(*n),
            Expr::Str(t) => Value::Text(t.clone()),
            Expr::Bool(b) => Value::Bool(*b),
            Expr::Ref(name, r, c) => match self.sheet_index(s, name) {
                Some(si) => match self.cell(si, *r, *c) {
                    Value::Empty => Value::Num(0.0),
                    v => v,
                },
                None => Value::Err(ERR_REF),
            },
            Expr::Range(..) => Value::Err(ERR_VALUE),
            Expr::Neg(x) => num1(self.eval(s, x), |n| -n),
            Expr::Pct(x) => num1(self.eval(s, x), |n| n / 100.0),
            Expr::Bin(op, a, b) => {
                let (va, vb) = (self.eval(s, a), self.eval(s, b));
                binary(op, va, vb)
            }
            Expr::Call(name, args) => self.call(s, name, args),
        }
    }

    fn call(&mut self, s: usize, name: &str, args: &[Expr]) -> Value {
        let n = args.len();
        let want = |lo: usize, hi: usize| n >= lo && n <= hi;
        let r: Result<Value, &'static str> = (|| match name {
            "SUM" => Ok(Value::Num(self.nums(s, args)?.iter().sum())),
            "PRODUCT" => Ok(Value::Num(self.nums(s, args)?.iter().product())),
            "AVERAGE" => {
                let v = self.nums(s, args)?;
                if v.is_empty() {
                    return Err(ERR_DIV);
                }
                Ok(Value::Num(v.iter().sum::<f64>() / v.len() as f64))
            }
            "MIN" => Ok(Value::Num(self.nums(s, args)?.into_iter().reduce(f64::min).unwrap_or(0.0))),
            "MAX" => Ok(Value::Num(self.nums(s, args)?.into_iter().reduce(f64::max).unwrap_or(0.0))),
            "COUNT" | "COUNTA" => {
                let mut count = 0;
                for a in args {
                    let vals = match self.arg(s, a) {
                        Arg::One(v) => Vec::from([v]),
                        Arg::Many(v) => v,
                    };
                    count += vals.iter().filter(|v| if name == "COUNT" { matches!(v, Value::Num(_)) } else { !matches!(v, Value::Empty) }).count();
                }
                Ok(Value::Num(count as f64))
            }
            "IF" if want(2, 3) => {
                let cond = truthy(self.eval(s, &args[0]))?;
                if cond {
                    Ok(self.eval(s, &args[1]))
                } else if n == 3 {
                    Ok(self.eval(s, &args[2]))
                } else {
                    Ok(Value::Bool(false))
                }
            }
            "IFERROR" if n == 2 => match self.eval(s, &args[0]) {
                Value::Err(_) => Ok(self.eval(s, &args[1])),
                v => Ok(v),
            },
            "ROUND" | "ROUNDUP" | "ROUNDDOWN" if n == 2 => {
                let x = self.eval(s, &args[0]).as_num()?;
                let d = libm::trunc(self.eval(s, &args[1]).as_num()?);
                let m = libm::pow(10.0, d);
                let y = x * m;
                let y = match name {
                    "ROUND" => libm::round(y),
                    "ROUNDUP" => {
                        if y >= 0.0 {
                            libm::ceil(y - 1e-9)
                        } else {
                            libm::floor(y + 1e-9)
                        }
                    }
                    _ => libm::trunc(y),
                };
                Ok(Value::Num(y / m))
            }
            "INT" if n == 1 => Ok(Value::Num(libm::floor(self.eval(s, &args[0]).as_num()?))),
            "ABS" if n == 1 => Ok(Value::Num(libm::fabs(self.eval(s, &args[0]).as_num()?))),
            "SQRT" if n == 1 => {
                let x = self.eval(s, &args[0]).as_num()?;
                if x < 0.0 {
                    return Err(ERR_NUM);
                }
                Ok(Value::Num(libm::sqrt(x)))
            }
            "POWER" if n == 2 => Ok(binary("^", self.eval(s, &args[0]), self.eval(s, &args[1]))),
            "MOD" if n == 2 => {
                let a = self.eval(s, &args[0]).as_num()?;
                let b = self.eval(s, &args[1]).as_num()?;
                if b == 0.0 {
                    return Err(ERR_DIV);
                }
                Ok(Value::Num(a - b * libm::floor(a / b)))
            }
            "AND" | "OR" => {
                let mut acc = name == "AND";
                for a in args {
                    let vals = match self.arg(s, a) {
                        Arg::One(v) => Vec::from([v]),
                        Arg::Many(v) => v,
                    };
                    for v in vals {
                        if matches!(v, Value::Empty | Value::Text(_)) {
                            continue;
                        }
                        let t = truthy(v)?;
                        acc = if name == "AND" { acc && t } else { acc || t };
                    }
                }
                Ok(Value::Bool(acc))
            }
            "NOT" if n == 1 => Ok(Value::Bool(!truthy(self.eval(s, &args[0]))?)),
            "CONCATENATE" | "CONCAT" => {
                let mut out = String::new();
                for a in args {
                    match self.arg(s, a) {
                        Arg::One(Value::Err(e)) => return Err(e),
                        Arg::One(v) => out += &v.as_text(),
                        Arg::Many(vs) => vs.iter().for_each(|v| out += &v.as_text()),
                    }
                }
                Ok(Value::Text(out))
            }
            "LEN" if n == 1 => Ok(Value::Num(text(self.eval(s, &args[0]))?.chars().count() as f64)),
            "UPPER" if n == 1 => Ok(Value::Text(text(self.eval(s, &args[0]))?.to_uppercase())),
            "LOWER" if n == 1 => Ok(Value::Text(text(self.eval(s, &args[0]))?.to_lowercase())),
            "TRIM" if n == 1 => {
                let t = text(self.eval(s, &args[0]))?;
                Ok(Value::Text(t.split_whitespace().collect::<Vec<_>>().join(" ")))
            }
            "LEFT" | "RIGHT" if want(1, 2) => {
                let t = text(self.eval(s, &args[0]))?;
                let k = if n == 2 { self.eval(s, &args[1]).as_num()? } else { 1.0 };
                if k < 0.0 {
                    return Err(ERR_VALUE);
                }
                let k = k as usize;
                let chars: Vec<char> = t.chars().collect();
                let part: String = if name == "LEFT" { chars.iter().take(k).collect() } else { chars[chars.len().saturating_sub(k)..].iter().collect() };
                Ok(Value::Text(part))
            }
            "MID" if n == 3 => {
                let t = text(self.eval(s, &args[0]))?;
                let st = self.eval(s, &args[1]).as_num()?;
                let k = self.eval(s, &args[2]).as_num()?;
                if st < 1.0 || k < 0.0 {
                    return Err(ERR_VALUE);
                }
                Ok(Value::Text(t.chars().skip(st as usize - 1).take(k as usize).collect()))
            }
            "TODAY" | "NOW" if n == 0 => Ok(Value::Num(today_serial())),
            "SUMIF" | "COUNTIF" if want(2, 3) => {
                let (Expr::Range(rn, (r0, c0), (r1, c1)), crit) = (&args[0], self.eval(s, &args[1])) else { return Err(ERR_VALUE) };
                let si = self.sheet_index(s, rn).ok_or(ERR_REF)?;
                let (mut sum, mut count) = (0.0, 0.0);
                for r in *r0..=*r1 {
                    for c in *c0..=*c1 {
                        if r - r0 > 100_000 {
                            break;
                        }
                        let v = self.cell(si, r, c);
                        if matches!(v, Value::Empty) || !criterion(&v, &crit) {
                            continue;
                        }
                        count += 1.0;
                        if name == "SUMIF" {
                            let target = match args.get(2) {
                                Some(Expr::Range(sn, (sr, sc), _)) => {
                                    let ssi = self.sheet_index(s, sn).ok_or(ERR_REF)?;
                                    self.cell(ssi, sr + (r - r0), sc + (c - c0))
                                }
                                Some(_) => return Err(ERR_VALUE),
                                None => v,
                            };
                            if let Value::Num(x) = target {
                                sum += x;
                            }
                        }
                    }
                }
                Ok(Value::Num(if name == "SUMIF" { sum } else { count }))
            }
            _ if KNOWN.contains(&name) => Err(ERR_VALUE), // wrong number of arguments
            _ => Err(ERR_NAME),
        })();
        r.unwrap_or_else(Value::Err)
    }
}

fn num1(v: Value, f: impl Fn(f64) -> f64) -> Value {
    match v.as_num() {
        Ok(n) => Value::Num(f(n)),
        Err(e) => Value::Err(e),
    }
}

fn text(v: Value) -> Result<String, &'static str> {
    match v {
        Value::Err(e) => Err(e),
        v => Ok(v.as_text()),
    }
}

fn truthy(v: Value) -> Result<bool, &'static str> {
    match v {
        Value::Bool(b) => Ok(b),
        Value::Num(n) => Ok(n != 0.0),
        Value::Empty => Ok(false),
        Value::Text(t) if t.eq_ignore_ascii_case("true") => Ok(true),
        Value::Text(t) if t.eq_ignore_ascii_case("false") => Ok(false),
        Value::Text(_) => Err(ERR_VALUE),
        Value::Err(e) => Err(e),
    }
}

/// Compare two values like Excel: numbers < text < booleans; text ignores case.
fn cmp(a: &Value, b: &Value) -> core::cmp::Ordering {
    use core::cmp::Ordering;
    let rank = |v: &Value| match v {
        Value::Num(_) | Value::Empty => 0,
        Value::Text(_) => 1,
        Value::Bool(_) => 2,
        Value::Err(_) => 3,
    };
    match (a, b) {
        (Value::Text(x), Value::Text(y)) => x.to_lowercase().cmp(&y.to_lowercase()),
        (Value::Bool(x), Value::Bool(y)) => x.cmp(y),
        (Value::Text(x), Value::Empty) => if x.is_empty() { Ordering::Equal } else { Ordering::Greater },
        (Value::Empty, Value::Text(y)) => if y.is_empty() { Ordering::Equal } else { Ordering::Less },
        _ if rank(a) == 0 && rank(b) == 0 => {
            let (x, y) = (a.as_num().unwrap_or(0.0), b.as_num().unwrap_or(0.0));
            x.partial_cmp(&y).unwrap_or(Ordering::Equal)
        }
        _ => rank(a).cmp(&rank(b)),
    }
}

fn binary(op: &str, a: Value, b: Value) -> Value {
    if let Value::Err(e) = a {
        return Value::Err(e);
    }
    if let Value::Err(e) = b {
        return Value::Err(e);
    }
    match op {
        "&" => Value::Text(a.as_text() + &b.as_text()),
        "=" | "<>" | "<" | ">" | "<=" | ">=" => {
            use core::cmp::Ordering::*;
            let o = cmp(&a, &b);
            Value::Bool(match op {
                "=" => o == Equal,
                "<>" => o != Equal,
                "<" => o == Less,
                ">" => o == Greater,
                "<=" => o != Greater,
                _ => o != Less,
            })
        }
        _ => {
            let (x, y) = match (a.as_num(), b.as_num()) {
                (Ok(x), Ok(y)) => (x, y),
                (Err(e), _) | (_, Err(e)) => return Value::Err(e),
            };
            let r = match op {
                "+" => x + y,
                "-" => x - y,
                "*" => x * y,
                "/" => {
                    if y == 0.0 {
                        return Value::Err(ERR_DIV);
                    }
                    x / y
                }
                _ => libm::pow(x, y),
            };
            if r.is_nan() || r.is_infinite() {
                Value::Err(ERR_NUM)
            } else {
                Value::Num(r)
            }
        }
    }
}

/// SUMIF/COUNTIF criteria: 5, "apples", ">10", "<>0", "=x".
fn criterion(v: &Value, crit: &Value) -> bool {
    let (op, rhs) = match crit {
        Value::Text(t) => {
            let (op, rest) = ["<=", ">=", "<>", "<", ">", "="].iter().find_map(|o| t.strip_prefix(o).map(|r| (*o, r))).unwrap_or(("=", t.as_str()));
            let rhs = match super::sheet::parse_number(rest) {
                Some((n, _)) => Value::Num(n),
                None => Value::Text(String::from(rest)),
            };
            (op, rhs)
        }
        other => ("=", other.clone()),
    };
    // Number criteria only match numbers (and text criteria only text), like Excel.
    if matches!(rhs, Value::Num(_)) != matches!(v, Value::Num(_)) {
        return op == "<>";
    }
    matches!(binary(op, v.clone(), rhs), Value::Bool(true))
}

/// Values of every formula cell in the workbook.
pub fn evaluate_all(wb: &Workbook) -> BTreeMap<(usize, u32, u32), Value> {
    let mut ev = Eval { wb, done: BTreeMap::new(), busy: Vec::new() };
    for (s, sheet) in wb.sheets.iter().enumerate() {
        let keys: Vec<(u32, u32)> = sheet.cells.iter().filter(|(_, c)| matches!(c.content, Content::Formula(..))).map(|(k, _)| *k).collect();
        for (r, c) in keys {
            ev.cell(s, r, c);
        }
    }
    ev.done
}

/// For tests and the log: evaluate a formula on its own sheet.
#[allow(dead_code)]
pub fn eval_text(wb: &Workbook, sheet: usize, f: &str) -> String {
    let mut ev = Eval { wb, done: BTreeMap::new(), busy: Vec::new() };
    match parse(f) {
        Ok(e) => match ev.eval(sheet, &e) {
            Value::Num(n) => general(n),
            v => v.as_text(),
        },
        Err(e) => String::from(e),
    }
}

/// Tidy a typed formula the way Excel does: function names and cell references in capitals
/// (text in quotes and sheet names are left alone).
pub fn normalize(f: &str) -> String {
    let b: Vec<char> = f.chars().collect();
    let mut out = String::new();
    let mut i = 0;
    while i < b.len() {
        let c = b[i];
        if c == '"' || c == '\'' {
            // Copy a quoted string or quoted sheet name as is.
            out.push(c);
            i += 1;
            while i < b.len() {
                out.push(b[i]);
                if b[i] == c {
                    if b.get(i + 1) == Some(&c) {
                        out.push(c);
                        i += 2;
                        continue;
                    }
                    i += 1;
                    break;
                }
                i += 1;
            }
            continue;
        }
        if c.is_ascii_alphabetic() || c == '_' || c == '$' {
            let st = i;
            while i < b.len() && (b[i].is_ascii_alphanumeric() || b[i] == '_' || b[i] == '$' || b[i] == '.') {
                i += 1;
            }
            let word: String = b[st..i].iter().collect();
            if b.get(i) == Some(&'!') {
                out.push_str(&word); // a sheet name
            } else {
                out.push_str(&word.to_ascii_uppercase());
            }
            continue;
        }
        out.push(c);
        i += 1;
    }
    out
}
