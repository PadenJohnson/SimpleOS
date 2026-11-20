//! A USB host controller of either kind: USB 3 (xHCI) or USB 2 (EHCI). The device code
//! (drives, printers, phones, hubs) talks to this and doesn't care which it is.

use alloc::vec::Vec;

use crate::ehci::Ehci;
use crate::memory::Dma;
use crate::xhci::{Completion, EpConfig, Speed, UsbError, Xhci};

pub enum Hc {
    X(Xhci),
    E(Ehci),
}

macro_rules! both {
    ($self:ident, $h:ident => $e:expr) => {
        match $self {
            Hc::X($h) => $e,
            Hc::E($h) => $e,
        }
    };
}

impl Hc {
    pub fn name(&self) -> &str {
        both!(self, h => &h.name)
    }
    pub fn kind(&self) -> &'static str {
        match self {
            Hc::X(_) => "USB 3",
            Hc::E(_) => "USB 2",
        }
    }
    /// Intel ports were moved over from the USB 2 controller.
    pub fn switched_ports(&self) -> bool {
        matches!(self, Hc::X(h) if h.switched_ports)
    }
    /// Give moved Intel ports back to the USB 2 controller.
    pub fn unswitch_ports(&mut self) {
        if let Hc::X(h) = self {
            h.unswitch_ports();
        }
    }
    pub fn ports(&self) -> u8 {
        both!(self, h => h.ports())
    }
    pub fn portsc(&self, p: u8) -> u32 {
        both!(self, h => h.portsc(p))
    }
    pub fn poll(&mut self) {
        both!(self, h => h.poll())
    }
    pub fn take_port_changes(&mut self) -> Vec<u8> {
        both!(self, h => h.take_port_changes())
    }
    pub fn port_connected(&self, p: u8) -> bool {
        both!(self, h => h.port_connected(p))
    }
    pub fn take_connect_change(&self, p: u8) -> bool {
        both!(self, h => h.take_connect_change(p))
    }
    pub fn reset_port(&mut self, p: u8) -> Option<Speed> {
        both!(self, h => h.reset_port(p))
    }
    pub fn enable_slot(&mut self) -> Result<u8, UsbError> {
        both!(self, h => h.enable_slot())
    }
    pub fn disable_slot(&mut self, slot: u8) {
        both!(self, h => h.disable_slot(slot))
    }
    pub fn has_slot(&self, slot: u8) -> bool {
        both!(self, h => h.has_slot(slot))
    }
    pub fn address_device(&mut self, slot: u8, port: u8, route: u32, speed: Speed, tt: Option<(u8, u8, bool)>) -> Result<(), UsbError> {
        both!(self, h => h.address_device(slot, port, route, speed, tt))
    }
    pub fn set_ep0_mps(&mut self, slot: u8, mps: u16) -> Result<(), UsbError> {
        both!(self, h => h.set_ep0_mps(slot, mps))
    }
    pub fn configure_endpoints(&mut self, slot: u8, eps: &[EpConfig], hub: Option<u8>) -> Result<(), UsbError> {
        both!(self, h => h.configure_endpoints(slot, eps, hub))
    }
    pub fn recover_endpoint(&mut self, slot: u8, dci: u8) -> Result<(), UsbError> {
        both!(self, h => h.recover_endpoint(slot, dci))
    }
    pub fn control(&mut self, slot: u8, request_type: u8, request: u8, value: u16, index: u16, data: Option<(&Dma, usize)>) -> Result<usize, UsbError> {
        both!(self, h => h.control(slot, request_type, request, value, index, data))
    }
    pub fn queue(&mut self, slot: u8, dci: u8, buf: u64, len: usize) -> Result<(), UsbError> {
        both!(self, h => h.queue(slot, dci, buf, len))
    }
    pub fn bulk(&mut self, slot: u8, dci: u8, buf: u64, len: usize, timeout_ms: u64) -> Result<usize, UsbError> {
        both!(self, h => h.bulk(slot, dci, buf, len, timeout_ms))
    }
    pub fn take_completion(&mut self, slot: u8, dci: u8) -> Option<Completion> {
        match self {
            Hc::X(h) => h.take_completion(slot, dci),
            Hc::E(_) => None,
        }
    }
    pub fn shutdown(&mut self) {
        both!(self, h => h.shutdown())
    }
}
