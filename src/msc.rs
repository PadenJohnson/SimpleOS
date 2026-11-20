//! USB mass storage: Bulk-Only Transport carrying SCSI commands. READ ONLY:
//! no write command is ever sent, so a plugged-in drive is never modified.

use alloc::string::String;

use crate::memory::Dma;
use crate::hc::Hc;
use crate::xhci::{dma_buf, UsbError};

/// Largest single READ: one DMA buffer, which must not cross a 64 KiB boundary.
pub const MAX_XFER: usize = 64 * 1024;

pub struct MassStorage {
    pub slot: u8,
    dci_in: u8,
    dci_out: u8,
    ep_in_addr: u8,
    ep_out_addr: u8,
    interface: u8,
    tag: u32,
    pub block_size: u32,
    pub blocks: u64,
    pub vendor: String,
    pub product: String,
    cbw: Dma,
    csw: Dma,
    data: Dma,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IoError {
    Usb(UsbError),
    /// The device reported the command failed (CSW status).
    CommandFailed,
    Protocol,
    NotReady,
}

impl From<UsbError> for IoError {
    fn from(e: UsbError) -> Self {
        IoError::Usb(e)
    }
}

fn ascii(b: &[u8]) -> String {
    let s: String = b.iter().map(|&c| if (0x20..0x7F).contains(&c) { c as char } else { ' ' }).collect();
    String::from(s.trim())
}

impl MassStorage {
    pub fn new(slot: u8, interface: u8, ep_in: u8, ep_out: u8) -> MassStorage {
        MassStorage {
            slot,
            dci_in: (ep_in & 0xF) * 2 + 1,
            dci_out: (ep_out & 0xF) * 2,
            ep_in_addr: ep_in,
            ep_out_addr: ep_out,
            interface,
            tag: 1,
            block_size: 512,
            blocks: 0,
            vendor: String::new(),
            product: String::new(),
            cbw: dma_buf(64),
            csw: dma_buf(64),
            data: Dma::new(MAX_XFER, MAX_XFER),
        }
    }

    /// Identify the drive and wait until it's ready.
    pub fn start(&mut self, hc: &mut Hc) -> Result<(), IoError> {
        // INQUIRY: vendor + product names.
        let mut cb = [0u8; 6];
        cb[0] = 0x12;
        cb[4] = 36;
        let n = self.command(hc, &cb, true, 36)?;
        if n >= 32 {
            let d = self.data.bytes();
            self.vendor = ascii(&d[8..16]);
            self.product = ascii(&d[16..32]);
        }

        // Card readers and slow sticks may report "not ready" for a while after plug-in.
        let mut ready = false;
        for _ in 0..20 {
            if self.command(hc, &[0u8; 6], false, 0).is_ok() {
                ready = true;
                break;
            }
            self.request_sense(hc);
            crate::interrupts::sleep_ms(100);
        }
        if !ready {
            return Err(IoError::NotReady);
        }

        // READ CAPACITY (10), falling back to (16) for drives over 2 TiB.
        let mut cb = [0u8; 10];
        cb[0] = 0x25;
        self.command(hc, &cb, true, 8)?;
        let d = self.data.bytes();
        let last = u32::from_be_bytes([d[0], d[1], d[2], d[3]]);
        self.block_size = u32::from_be_bytes([d[4], d[5], d[6], d[7]]);
        self.blocks = last as u64 + 1;
        if last == 0xFFFF_FFFF {
            let mut cb = [0u8; 16];
            cb[0] = 0x9E;
            cb[1] = 0x10;
            cb[13] = 32;
            self.command(hc, &cb, true, 32)?;
            let d = self.data.bytes();
            self.blocks = u64::from_be_bytes(d[0..8].try_into().unwrap()) + 1;
            self.block_size = u32::from_be_bytes(d[8..12].try_into().unwrap());
        }
        if !(512..=4096).contains(&self.block_size) || !self.block_size.is_power_of_two() {
            return Err(IoError::Protocol);
        }
        Ok(())
    }

    fn request_sense(&mut self, hc: &mut Hc) {
        let mut cb = [0u8; 6];
        cb[0] = 0x03;
        cb[4] = 18;
        let _ = self.command(hc, &cb, true, 18);
    }

    /// Read `out.len() / block_size` blocks starting at `lba`.
    pub fn read(&mut self, hc: &mut Hc, lba: u64, out: &mut [u8]) -> Result<(), IoError> {
        let bs = self.block_size as usize;
        let per = MAX_XFER / bs;
        let mut done = 0usize;
        let total = out.len() / bs;
        while done < total {
            let n = (total - done).min(per);
            let at = lba + done as u64;
            let mut attempt = 0;
            loop {
                let r = if at + n as u64 <= u32::MAX as u64 {
                    let mut cb = [0u8; 10];
                    cb[0] = 0x28; // READ (10)
                    cb[2..6].copy_from_slice(&(at as u32).to_be_bytes());
                    cb[7..9].copy_from_slice(&(n as u16).to_be_bytes());
                    self.command(hc, &cb, true, n * bs)
                } else {
                    let mut cb = [0u8; 16];
                    cb[0] = 0x88; // READ (16)
                    cb[2..10].copy_from_slice(&at.to_be_bytes());
                    cb[10..14].copy_from_slice(&(n as u32).to_be_bytes());
                    self.command(hc, &cb, true, n * bs)
                };
                match r {
                    Ok(got) if got >= n * bs => break,
                    Ok(_) => return Err(IoError::Protocol),
                    Err(IoError::Usb(UsbError::NoDevice)) => return Err(IoError::Usb(UsbError::NoDevice)),
                    Err(e) => {
                        attempt += 1;
                        if attempt >= 3 {
                            return Err(e);
                        }
                        self.request_sense(hc);
                    }
                }
            }
            out[done * bs..(done + n) * bs].copy_from_slice(&self.data.bytes()[..n * bs]);
            done += n;
        }
        Ok(())
    }

    /// Write blocks. Only for Solon's own boot stick (`usb::Usb::boot_disk`) and, when the user
    /// asks, Copy to USB (`usb_create`, add-only) and Erase (`format_drive`, never the boot stick).
    pub fn write(&mut self, hc: &mut Hc, lba: u64, data: &[u8]) -> Result<(), IoError> {
        let bs = self.block_size as usize;
        let per = MAX_XFER / bs;
        let total = data.len() / bs;
        let mut done = 0usize;
        while done < total {
            let n = (total - done).min(per);
            let at = lba + done as u64;
            self.data.bytes_mut()[..n * bs].copy_from_slice(&data[done * bs..(done + n) * bs]);
            let mut attempt = 0;
            loop {
                let r = if at + n as u64 <= u32::MAX as u64 {
                    let mut cb = [0u8; 10];
                    cb[0] = 0x2A; // WRITE (10)
                    cb[2..6].copy_from_slice(&(at as u32).to_be_bytes());
                    cb[7..9].copy_from_slice(&(n as u16).to_be_bytes());
                    self.command(hc, &cb, false, n * bs)
                } else {
                    let mut cb = [0u8; 16];
                    cb[0] = 0x8A; // WRITE (16)
                    cb[2..10].copy_from_slice(&at.to_be_bytes());
                    cb[10..14].copy_from_slice(&(n as u32).to_be_bytes());
                    self.command(hc, &cb, false, n * bs)
                };
                match r {
                    Ok(_) => break,
                    Err(IoError::Usb(UsbError::NoDevice)) => return Err(IoError::Usb(UsbError::NoDevice)),
                    Err(e) => {
                        attempt += 1;
                        if attempt >= 3 {
                            return Err(e);
                        }
                        self.request_sense(hc);
                    }
                }
            }
            done += n;
        }
        Ok(())
    }

    /// SYNCHRONIZE CACHE: make sure written data is really on the stick.
    pub fn flush(&mut self, hc: &mut Hc) -> Result<(), IoError> {
        let mut cb = [0u8; 10];
        cb[0] = 0x35;
        match self.command(hc, &cb, false, 0) {
            Err(IoError::CommandFailed) => Ok(()), // some sticks don't support it; they write through
            r => r.map(|_| ()),
        }
    }

    /// One Bulk-Only Transport command: CBW out, optional data in, CSW in.
    /// Returns the number of data bytes received.
    fn command(&mut self, hc: &mut Hc, cb: &[u8], dir_in: bool, len: usize) -> Result<usize, IoError> {
        if !hc.has_slot(self.slot) {
            return Err(IoError::Usb(UsbError::NoDevice));
        }
        let tag = self.tag;
        self.tag = self.tag.wrapping_add(1);
        {
            let w = self.cbw.bytes_mut();
            w[..31].fill(0);
            w[0..4].copy_from_slice(&0x4342_5355u32.to_le_bytes()); // "USBC"
            w[4..8].copy_from_slice(&tag.to_le_bytes());
            w[8..12].copy_from_slice(&(len as u32).to_le_bytes());
            w[12] = if dir_in { 0x80 } else { 0 };
            w[13] = 0; // LUN 0
            w[14] = cb.len() as u8;
            w[15..15 + cb.len()].copy_from_slice(cb);
        }
        if let Err(e) = hc.bulk(self.slot, self.dci_out, self.cbw.phys(), 31, 2000) {
            self.reset_recovery(hc);
            return Err(e.into());
        }

        let mut got = 0;
        if len > 0 && !dir_in {
            // Data out (writes; only ever used for Solon's own boot stick).
            match hc.bulk(self.slot, self.dci_out, self.data.phys(), len, 5000) {
                Ok(n) => got = n,
                Err(UsbError::Stall) => self.clear_halt(hc, self.dci_out, self.ep_out_addr),
                Err(e) => {
                    self.reset_recovery(hc);
                    return Err(e.into());
                }
            }
        } else if len > 0 {
            match hc.bulk(self.slot, self.dci_in, self.data.phys(), len, 5000) {
                Ok(n) => got = n,
                Err(UsbError::Stall) => self.clear_halt(hc, self.dci_in, self.ep_in_addr),
                Err(e) => {
                    self.reset_recovery(hc);
                    return Err(e.into());
                }
            }
        }

        // Status. A stall here is cleared once and retried.
        let mut status = hc.bulk(self.slot, self.dci_in, self.csw.phys(), 13, 2000);
        if status == Err(UsbError::Stall) {
            self.clear_halt(hc, self.dci_in, self.ep_in_addr);
            status = hc.bulk(self.slot, self.dci_in, self.csw.phys(), 13, 2000);
        }
        if let Err(e) = status {
            self.reset_recovery(hc);
            return Err(e.into());
        }
        let c = self.csw.bytes();
        if u32::from_le_bytes([c[0], c[1], c[2], c[3]]) != 0x5342_5355 || u32::from_le_bytes([c[4], c[5], c[6], c[7]]) != tag {
            self.reset_recovery(hc);
            return Err(IoError::Protocol);
        }
        match c[12] {
            0 => Ok(got),
            1 => Err(IoError::CommandFailed),
            _ => {
                self.reset_recovery(hc);
                Err(IoError::Protocol)
            }
        }
    }

    fn clear_halt(&mut self, hc: &mut Hc, dci: u8, ep_addr: u8) {
        let _ = hc.recover_endpoint(self.slot, dci);
        let _ = hc.control(self.slot, 0x02, 0x01, 0, ep_addr as u16, None); // CLEAR_FEATURE(ENDPOINT_HALT)
    }

    /// Bulk-Only Mass Storage Reset, then clear both endpoints.
    fn reset_recovery(&mut self, hc: &mut Hc) {
        if !hc.has_slot(self.slot) {
            return;
        }
        let _ = hc.control(self.slot, 0x21, 0xFF, 0, self.interface as u16, None);
        self.clear_halt(hc, self.dci_in, self.ep_in_addr);
        self.clear_halt(hc, self.dci_out, self.ep_out_addr);
    }

    pub fn size_bytes(&self) -> u64 {
        self.blocks * self.block_size as u64
    }
}
