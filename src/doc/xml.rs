//! A small XML reader (enough for Office files) and an escaper for writing.

use alloc::string::String;
use alloc::vec::Vec;

pub enum Ev<'a> {
    /// <name attrs...> ; `empty` for <name/>
    Start { name: &'a str, attrs: Vec<(&'a str, String)>, empty: bool },
    End(&'a str),
    Text(String),
}

pub struct Reader<'a> {
    s: &'a str,
    p: usize,
}

/// Name without its namespace prefix ("x:row" -> "row").
pub fn local(name: &str) -> &str {
    name.rsplit(':').next().unwrap_or(name)
}

pub fn unescape(s: &str) -> String {
    if !s.contains('&') {
        return String::from(s);
    }
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find('&') {
        out.push_str(&rest[..i]);
        rest = &rest[i..];
        let Some(end) = rest.find(';') else { break };
        let ent = &rest[1..end];
        let ch = match ent {
            "amp" => Some('&'),
            "lt" => Some('<'),
            "gt" => Some('>'),
            "quot" => Some('"'),
            "apos" => Some('\''),
            _ if ent.starts_with("#x") || ent.starts_with("#X") => u32::from_str_radix(&ent[2..], 16).ok().and_then(char::from_u32),
            _ if ent.starts_with('#') => ent[1..].parse().ok().and_then(char::from_u32),
            _ => None,
        };
        match ch {
            Some(c) => {
                out.push(c);
                rest = &rest[end + 1..];
            }
            None => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

pub fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            c if (c as u32) < 0x20 && c != '\n' && c != '\t' && c != '\r' => {}
            c => out.push(c),
        }
    }
    out
}

impl<'a> Reader<'a> {
    pub fn new(bytes: &'a [u8]) -> Reader<'a> {
        let s = core::str::from_utf8(bytes).unwrap_or("");
        let s = s.strip_prefix('\u{feff}').unwrap_or(s);
        Reader { s, p: 0 }
    }

    pub fn next(&mut self) -> Option<Ev<'a>> {
        loop {
            let rest = &self.s[self.p..];
            if rest.is_empty() {
                return None;
            }
            if !rest.starts_with('<') {
                let end = rest.find('<').unwrap_or(rest.len());
                self.p += end;
                return Some(Ev::Text(unescape(&rest[..end])));
            }
            if rest.starts_with("<!--") {
                self.p += rest.find("-->").map(|i| i + 3).unwrap_or(rest.len());
                continue;
            }
            if let Some(cd) = rest.strip_prefix("<![CDATA[") {
                let end = cd.find("]]>").unwrap_or(cd.len());
                self.p += 9 + end + 3.min(cd.len() - end);
                return Some(Ev::Text(String::from(&cd[..end])));
            }
            if rest.starts_with("<?") || rest.starts_with("<!") {
                self.p += rest.find('>').map(|i| i + 1).unwrap_or(rest.len());
                continue;
            }
            // A tag: find its end, skipping '>' inside quoted attribute values.
            let bytes = rest.as_bytes();
            let mut i = 1;
            let mut quote = 0u8;
            while i < bytes.len() {
                let c = bytes[i];
                if quote != 0 {
                    if c == quote {
                        quote = 0;
                    }
                } else if c == b'"' || c == b'\'' {
                    quote = c;
                } else if c == b'>' {
                    break;
                }
                i += 1;
            }
            let tag = &rest[1..i.min(rest.len())];
            self.p += (i + 1).min(rest.len());
            if let Some(name) = tag.strip_prefix('/') {
                return Some(Ev::End(name.trim()));
            }
            let empty = tag.ends_with('/');
            let tag = tag.trim_end_matches('/');
            let name_end = tag.find(|c: char| c.is_whitespace()).unwrap_or(tag.len());
            let name = &tag[..name_end];
            let mut attrs = Vec::new();
            let mut a = &tag[name_end..];
            loop {
                a = a.trim_start();
                let Some(eq) = a.find('=') else { break };
                let key = a[..eq].trim();
                let after = a[eq + 1..].trim_start();
                let Some(q) = after.chars().next() else { break };
                if q != '"' && q != '\'' {
                    break;
                }
                let Some(close) = after[1..].find(q) else { break };
                attrs.push((key, unescape(&after[1..1 + close])));
                a = &after[close + 2..];
            }
            return Some(Ev::Start { name, attrs, empty });
        }
    }
}

pub fn attr<'b>(attrs: &'b [(&str, String)], key: &str) -> Option<&'b str> {
    attrs.iter().find(|(k, _)| local(k) == key).map(|(_, v)| v.as_str())
}
