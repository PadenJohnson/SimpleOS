//! USB printers (USB printer class 7). Two ways of talking to them:
//! * IPP over USB (interface protocol 4): HTTP requests carrying IPP over bulk endpoints.
//! * Raw (protocols 1/2): the print job bytes go straight to the bulk OUT endpoint (PCL).

use alloc::string::String;
use alloc::vec::Vec;
use core::sync::atomic::Ordering::Relaxed;

use crate::interrupts::{HZ, TICKS};
use crate::memory::Dma;
use crate::print::{self, IppInfo};
use crate::xhci::{dma_buf, UsbError, Xhci};

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
    pub fn new(hc: &mut Xhci, id: u32, ctrl: usize, slot: u8, iface: u8, alt: u8, proto: u8, ep_out: u8, ep_in: Option<u8>, product: &str) -> UsbPrinter {
        // IEEE 1284 device ID: who the printer is and which languages it speaks.
        let buf = dma_buf(1024);
        let mut idstr = String::new();
        if hc.control(slot, 0xA1, 0, 0, (iface as u16) << 8 | alt as u16, Some((&buf, 1023))).is_ok() {
            let b = buf.bytes();
            let len = (u16::from_be_bytes([b[0], b[1]]) as usize).clamp(2, 1023);
            idstr = b[2..len].iter().map(|&c| if (0x20..0x7F).contains(&c) { c as char } else { ' ' }).collect();
        }
        let (mfg, mdl, langs) = parse_device_id(&idstr);
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
                        if !info.formats.iter().any(|f| f == "image/pwg-raster") {
                            kprintln!("usb:   printer doesn't accept PWG Raster; falling back to raw");
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
        p
    }

    pub fn speaks_pcl(&self) -> bool {
        self.langs.iter().any(|l| l.starts_with("PCL"))
    }

    /// Can we print to it at all?
    pub fn usable(&self) -> bool {
        self.ipp || self.speaks_pcl()
    }

    pub fn color(&self) -> bool {
        match &self.info {
            Some(i) if self.ipp => i.color_modes.iter().any(|m| m == "color"),
            _ => self.name.to_ascii_lowercase().contains("color") || self.langs.iter().any(|l| l == "PCL5C"),
        }
    }

    pub fn dpi(&self) -> u32 {
        match &self.info {
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
                _ => p.pcl_ok(),
            })
            .collect()
    }

    /// Send bytes to the printer's bulk OUT endpoint.
    pub fn send(&mut self, hc: &mut Xhci, data: &[u8]) -> Result<(), UsbError> {
        for chunk in data.chunks(64 * 1024) {
            self.buf.bytes_mut()[..chunk.len()].copy_from_slice(chunk);
            // A printer that's busy (warming up, buffer full) just makes us wait.
            hc.bulk(self.slot, self.dci_out, self.buf.phys(), chunk.len(), 60_000)?;
        }
        Ok(())
    }

    /// One HTTP/IPP exchange over USB. Returns the IPP response body.
    pub fn ipp_request(&mut self, hc: &mut Xhci, ipp: &[u8]) -> Result<Vec<u8>, &'static str> {
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
