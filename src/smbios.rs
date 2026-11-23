//! SMBIOS: what the firmware says the computer is made of (DMTF DSP0134). Read once at
//! start: the BIOS (type 0), the system and motherboard (types 1 and 2), the processor
//! socket (type 4) and each memory slot (type 17). Serial numbers are left alone.

use alloc::string::String;
use alloc::vec::Vec;
use spin::Once;

/// One memory slot.
#[derive(Clone, Debug, Default)]
pub struct Dimm {
    /// Where it is ("DIMM_A2"), and its bank ("BANK 1").
    pub slot: String,
    pub bank: String,
    /// Size in MiB (0: empty slot).
    pub mib: u64,
    pub kind: &'static str,
    /// Rated and running speeds (MT/s; 0: not given).
    pub speed: u32,
    pub configured: u32,
    pub maker: String,
    pub part: String,
}

#[derive(Clone, Debug, Default)]
pub struct Board {
    pub bios_vendor: String,
    pub bios_version: String,
    pub bios_date: String,
    pub system_maker: String,
    pub system_product: String,
    pub board_maker: String,
    pub board_product: String,
    pub board_version: String,
    pub socket: String,
    /// The processor's top speed as the firmware lists it (MHz).
    pub cpu_max_mhz: u32,
    pub dimms: Vec<Dimm>,
}

static BOARD: Once<Board> = Once::new();

/// What the firmware said (None if it had no SMBIOS tables).
pub fn get() -> Option<&'static Board> {
    BOARD.get()
}

fn phys(addr: u64, len: usize) -> Vec<u8> {
    let v = crate::memory::map_mmio(addr, len as u64);
    (0..len).map(|i| unsafe { core::ptr::read_volatile((v + i as u64) as *const u8) }).collect()
}

/// Read the tables from the entry point at physical address `entry`.
pub fn init(entry: u64) {
    let head = phys(entry, 32);
    let (table, len) = if &head[0..5] == b"_SM3_" {
        let len = u32::from_le_bytes([head[12], head[13], head[14], head[15]]) as usize;
        let addr = u64::from_le_bytes(head[16..24].try_into().unwrap_or([0; 8]));
        (addr, len)
    } else if &head[0..4] == b"_SM_" {
        let len = u16::from_le_bytes([head[0x16], head[0x17]]) as usize;
        let addr = u32::from_le_bytes([head[0x18], head[0x19], head[0x1a], head[0x1b]]) as u64;
        (addr, len)
    } else {
        kprintln!("smbios: no entry point at {:#x}", entry);
        return;
    };
    if table == 0 || len == 0 || len > 1024 * 1024 {
        kprintln!("smbios: table at {:#x}, {} bytes: not used", table, len);
        return;
    }
    let t = phys(table, len);
    let b = parse(&t);
    let join = |parts: &[&str]| parts.iter().filter(|p| !p.is_empty()).copied().collect::<Vec<_>>().join(" ");
    kprintln!(
        "smbios: system \"{}\", board \"{}\", BIOS \"{}\" ({}), socket \"{}\"",
        join(&[&b.system_maker, &b.system_product]), join(&[&b.board_maker, &b.board_product, &b.board_version]),
        join(&[&b.bios_vendor, &b.bios_version]), b.bios_date, b.socket
    );
    for d in b.dimms.iter().filter(|d| d.mib > 0) {
        kprintln!("smbios:   {} {}: {} MiB {} {} MT/s (running at {}), {} {}", d.bank, d.slot, d.mib, d.kind, d.speed, d.configured, d.maker, d.part);
    }
    BOARD.call_once(|| b);
}

/// The n-th (from 1) string after a structure's formatted part.
fn string(strings: &[u8], n: u8) -> String {
    if n == 0 {
        return String::new();
    }
    let s = strings.split(|&c| c == 0).nth(n as usize - 1).unwrap_or(&[]);
    let s: String = s.iter().map(|&c| if (0x20..0x7f).contains(&c) { c as char } else { '?' }).collect();
    let s = s.trim();
    // Placeholders some firmware leaves in.
    let lower = s.to_ascii_lowercase();
    if ["to be filled by o.e.m.", "default string", "not specified", "unknown", "none", "system product name", "system manufacturer", "undefined"].contains(&lower.as_str()) {
        String::new()
    } else {
        String::from(s)
    }
}

fn parse(t: &[u8]) -> Board {
    let mut b = Board::default();
    let mut at = 0;
    while at + 4 <= t.len() {
        let (ty, len) = (t[at], t[at + 1] as usize);
        if len < 4 || at + len > t.len() {
            break;
        }
        let f = &t[at..at + len];
        // The strings end with two zero bytes.
        let mut end = at + len;
        while end + 1 < t.len() && !(t[end] == 0 && t[end + 1] == 0) {
            end += 1;
        }
        let strings = &t[at + len..end.min(t.len())];
        let byte = |o: usize| f.get(o).copied().unwrap_or(0);
        let word = |o: usize| if o + 1 < f.len() { u16::from_le_bytes([f[o], f[o + 1]]) } else { 0 };
        let s = |o: usize| string(strings, byte(o));
        match ty {
            0 => {
                b.bios_vendor = s(4);
                b.bios_version = s(5);
                b.bios_date = s(8);
            }
            1 => {
                b.system_maker = s(4);
                b.system_product = s(5);
            }
            2 => {
                b.board_maker = s(4);
                b.board_product = s(5);
                b.board_version = s(6);
            }
            4 if b.socket.is_empty() => {
                b.socket = s(4);
                b.cpu_max_mhz = word(0x14) as u32;
            }
            17 => {
                let size = word(0x0c);
                let mib = match size {
                    0 | 0xffff => 0,
                    0x7fff if len > 0x1f => u32::from_le_bytes([byte(0x1c), byte(0x1d), byte(0x1e), byte(0x1f)]) as u64 & 0x7fff_ffff,
                    v if v & 0x8000 != 0 => (v & 0x7fff) as u64 / 1024,
                    v => v as u64,
                };
                let kind = match byte(0x12) {
                    0x18 => "DDR3",
                    0x1a => "DDR4",
                    0x1b => "LPDDR",
                    0x1d => "LPDDR3",
                    0x1e => "LPDDR4",
                    0x22 => "DDR5",
                    0x23 => "LPDDR5",
                    _ => "",
                };
                b.dimms.push(Dimm {
                    slot: s(0x10),
                    bank: s(0x11),
                    mib,
                    kind,
                    speed: word(0x15) as u32,
                    configured: if len > 0x21 { word(0x20) as u32 } else { 0 },
                    maker: s(0x17),
                    part: s(0x1a),
                });
            }
            127 => break,
            _ => {}
        }
        at = end + 2;
    }
    b
}
