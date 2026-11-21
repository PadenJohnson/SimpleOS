//! USB printers (USB printer class 7). Two ways of talking to them:
//! * IPP over USB (interface protocol 4): HTTP requests carrying IPP over bulk endpoints.
//! * Raw (protocols 1/2): the print job bytes go straight to the bulk OUT endpoint (PCL).

use alloc::string::String;
use alloc::vec::Vec;
use core::sync::atomic::Ordering::Relaxed;

use crate::interrupts::{HZ, TICKS};
use crate::memory::Dma;
use crate::print::{self, IppInfo};
use crate::hc::Hc;
use crate::xhci::{dma_buf, UsbError};

pub struct UsbPrinter {
    pub id: u32,
    pub ctrl: usize,
    pub slot: u8,
    dci_out: u8,
    dci_in: Option<u8>,
    /// Talks IPP over USB.
    pub ipp: bool,
    pub name: String,
    /// Languages from the IEEE 1284 device ID ("CMD:"), upper-case.
    pub langs: Vec<String>,
    pub info: Option<IppInfo>,
    /// Maker and model from the device ID.
    pub make: String,
    pub model: String,
    /// The language Solon will use, or why it can't print.
    pub lang: Option<print::Lang>,
    pub problem: Option<&'static str>,
    buf: Dma,
}

/// Parse "MFG:HP;MDL:LaserJet 1020;CMD:PJL,PCL;" into (make, model, languages).
fn parse_device_id(s: &str) -> (String, String, Vec<String>) {
    let (mut mfg, mut mdl, mut cmd) = (String::new(), String::new(), Vec::new());
    for part in s.split(';') {
        let Some((k, v)) = part.split_once(':') else { continue };
        match k.trim().to_ascii_uppercase().as_str() {
            "MFG" | "MANUFACTURER" => mfg = String::from(v.trim()),
            "MDL" | "MODEL" => mdl = String::from(v.trim()),
            "CMD" | "COMMAND SET" => cmd = v.split(',').map(|l| String::from(l.trim()).to_ascii_uppercase()).collect(),
            _ => {}
        }
    }
    (mfg, mdl, cmd)
}

impl UsbPrinter {
    /// Set up a printer interface that has already had its endpoints configured.
    pub fn new(hc: &mut Hc, id: u32, ctrl: usize, slot: u8, iface: u8, alt: u8, proto: u8, ep_out: u8, ep_in: Option<u8>, product: &str) -> UsbPrinter {
        // IEEE 1284 device ID: who the printer is and which languages it speaks.
        let buf = dma_buf(1024);
        let mut idstr = String::new();
        if hc.control(slot, 0xA1, 0, 0, (iface as u16) << 8 | alt as u16, Some((&buf, 1023))).is_ok() {
            let b = buf.bytes();
            let len = (u16::from_be_bytes([b[0], b[1]]) as usize).clamp(2, 1023);
            idstr = b[2..len].iter().map(|&c| if (0x20..0x7F).contains(&c) { c as char } else { ' ' }).collect();
        }
        let (mfg, mdl, langs) = parse_device_id(&idstr);
        let model_name = mdl.clone();
        let name = if mdl.is_empty() {
            String::from(if product.is_empty() { "USB printer" } else { product })
        } else if mdl.to_ascii_lowercase().starts_with(&mfg.to_ascii_lowercase()) || mfg.is_empty() {
            mdl
        } else {
            alloc::format!("{} {}", mfg, mdl)
        };
        let mut p = UsbPrinter {
            id,
            ctrl,
            slot,
            dci_out: (ep_out & 0xF) * 2,
            dci_in: ep_in.map(|e| (e & 0xF) * 2 + 1),
            ipp: proto == 4 && ep_in.is_some(),
            name,
            langs,
            info: None,
            make: mfg.clone(),
            model: String::new(),
            lang: None,
            problem: None,
            buf: Dma::new(64 * 1024, 64 * 1024),
        };
        if p.ipp {
            match p.ipp_request(hc, &print::ipp_get_attributes()) {
                Ok(body) => {
                    if let Some(info) = print::ipp_parse(&body) {
                        if !info.model.is_empty() {
                            p.name = info.model.clone();
                        }
                        kprintln!(
                            "usb:   IPP printer: formats {:?}, {} media, color modes {:?}, resolutions {:?}",
                            info.formats,
                            info.media.len(),
                            info.color_modes,
                            info.resolutions
                        );
                        if info.formats.iter().any(|f| f == "image/pwg-raster") {
                            p.lang = Some(print::Lang::Pwg);
                        } else if info.formats.iter().any(|f| f == "image/urf") {
                            p.lang = Some(print::Lang::Urf);
                        } else {
                            kprintln!("usb:   printer takes neither PWG nor Apple raster; trying raw");
                            p.ipp = false;
                        }
                        p.info = Some(info);
                    }
                }
                Err(e) => {
                    kprintln!("usb:   IPP query failed ({}); using raw printing", e);
                    p.ipp = false;
                }
            }
        }
        p.model = model_name;
        if p.lang.is_none() {
            p.ipp = false;
            p.lang = raw_language(&p.langs);
        }
        if p.lang.is_none() {
            p.problem = Some(why_not(&p.make, &p.langs));
        }
        p
    }

    /// Can we print to it at all?
    pub fn usable(&self) -> bool {
        self.lang.is_some()
    }

    pub fn color(&self) -> bool {
        match &self.info {
            Some(i) if self.ipp => i.color_modes.iter().any(|m| m == "color"),
            _ => {
                let n = self.name.to_ascii_lowercase();
                n.contains("color") || n.contains("colour") || self.langs.iter().any(|l| l == "PCL5C" || l == "PCL3GUI" || l == "PCL3")
            }
        }
    }

    pub fn dpi(&self) -> u32 {
        match &self.info {
            Some(i) if self.lang == Some(print::Lang::Urf) => {
                // "RS300" or "RS300-600": the resolutions it takes.
                let rs = i.urf.iter().find_map(|u| u.strip_prefix("RS")).and_then(|r| r.split('-').next()?.parse::<u32>().ok());
                rs.unwrap_or(300).clamp(150, 600)
            }
            Some(i) if self.ipp && !i.resolutions.is_empty() => {
                if i.resolutions.contains(&300) {
                    300
                } else {
                    i.resolutions[0].clamp(150, 600)
                }
            }
            _ => print::DPI,
        }
    }

    pub fn papers(&self) -> Vec<print::Paper> {
        print::Paper::ALL
            .iter()
            .copied()
            .filter(|p| match &self.info {
                Some(i) if self.ipp => i.media.is_empty() || i.media.iter().any(|m| m == p.pwg()),
                _ => self.lang == Some(print::Lang::PostScript) || p.pcl_ok(),
            })
            .collect()
    }

    /// Send bytes to the printer's bulk OUT endpoint.
    pub fn send(&mut self, hc: &mut Hc, data: &[u8]) -> Result<(), UsbError> {
        for chunk in data.chunks(64 * 1024) {
            self.buf.bytes_mut()[..chunk.len()].copy_from_slice(chunk);
            // A printer that's busy (warming up, buffer full) just makes us wait.
            hc.bulk(self.slot, self.dci_out, self.buf.phys(), chunk.len(), 60_000)?;
        }
        Ok(())
    }

    /// One HTTP/IPP exchange over USB. Returns the IPP response body.
    pub fn ipp_request(&mut self, hc: &mut Hc, ipp: &[u8]) -> Result<Vec<u8>, &'static str> {
        let din = self.dci_in.ok_or("printer can't answer")?;
        self.send(hc, &print::http_post(ipp)).map_err(|_| "couldn't send to the printer")?;
        let mut resp = Vec::new();
        let end = TICKS.load(Relaxed) + 60 * HZ;
        loop {
            match hc.bulk(self.slot, din, self.buf.phys(), 64 * 1024, 30_000) {
                Ok(n) => resp.extend_from_slice(&self.buf.bytes()[..n]),
                Err(UsbError::Timeout) => return Err("the printer didn't answer"),
                Err(_) => return Err("lost contact with the printer"),
            }
            if let Some((status, body)) = print::http_response(&resp) {
                if status != 200 {
                    kprintln!("usb: printer HTTP status {}", status);
                    return Err("the printer refused the request");
                }
                let head_end = resp.windows(4).position(|w| w == b"\r\n\r\n").unwrap_or(0);
                let chunked = core::str::from_utf8(&resp[..head_end])
                    .map(|s| s.to_ascii_lowercase().contains("transfer-encoding: chunked"))
                    .unwrap_or(false);
                return Ok(if chunked { print::dechunk(body) } else { Vec::from(body) });
            }
            if TICKS.load(Relaxed) > end {
                return Err("the printer didn't answer");
            }
        }
    }
}

/// The language to use for a printer that isn't driverless, from what its device ID lists.
fn raw_language(langs: &[String]) -> Option<print::Lang> {
    let has = |f: &dyn Fn(&str) -> bool| langs.iter().any(|l| f(l));
    if has(&|l| (l.starts_with("PCL") && !l.starts_with("PCLXL") && !l.starts_with("PCL6") && l != "PCLM") || l == "HPGL2") {
        Some(print::Lang::Pcl)
    } else if has(&|l| l.starts_with("PCLXL") || l.starts_with("PCL6") || l == "PXL") {
        Some(print::Lang::PclXl)
    } else if has(&|l| l.contains("POSTSCRIPT") || l == "PS" || l == "PS2" || l == "PS3" || l.contains("BR-SCRIPT") || l.contains("BRSCRIPT") || l == "KPDL") {
        Some(print::Lang::PostScript)
    } else {
        None
    }
}

/// Why a printer can't be used, in words people understand.
fn why_not(make: &str, langs: &[String]) -> &'static str {
    let m = make.to_ascii_lowercase();
    let has = |k: &str| langs.iter().any(|l| l.contains(k));
    if has("ZJS") || has("ZJSTREAM") || has("ZJ") {
        "This HP printer is a \"host-based\" model: it only works with HP's own software, which isn't available for Solon."
    } else if has("CAPT") || has("UFR") || has("LIPS") {
        "This Canon printer only speaks Canon's own language (CAPT/UFR), which Solon can't use."
    } else if has("SPL") || has("QPDL") || has("GDI") || has("SPLC") {
        "This printer uses Samsung/Xerox's own \"GDI\" language, which Solon can't use."
    } else if m.contains("epson") || has("ESCPR") || has("ESCPL2") {
        "This Epson printer only speaks Epson's own language over USB. Epson models with AirPrint or Mopria (most from 2016 on) work."
    } else if m.contains("canon") || has("BJRASTER") || has("BJL") || has("IVEC") {
        "This Canon printer only speaks Canon's own language over USB. Canon models with AirPrint or Mopria work."
    } else if m.contains("brother") {
        "This Brother printer only speaks Brother's own language over USB. Brother models with PCL, BR-Script or AirPrint work."
    } else if m.contains("lexmark") {
        "This Lexmark printer only speaks Lexmark's own language. Lexmark lasers (PCL or PostScript) work."
    } else if m.contains("dymo") || m.contains("zebra") {
        "Label printers aren't supported yet."
    } else {
        "This printer didn't say it speaks any language Solon knows (driverless/AirPrint, PCL, PCL 6 or PostScript)."
    }
}