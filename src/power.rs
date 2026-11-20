//! Power off and restart via ACPI (with fallbacks).
//!
//! Shut down = write the "soft off" (S5) sleep type into the PM1 control register(s), or the
//! sleep control register on "hardware-reduced" ACPI machines. The S5 values come from the
//! `_S5_` package in the DSDT. Restart = the ACPI reset register, then the keyboard
//! controller, then a deliberate triple fault.

use alloc::vec::Vec;
use x86_64::instructions::port::Port;

use crate::memory::map_mmio;

/// ACPI "generic address": where a register lives (I/O port or memory).
#[derive(Clone, Copy, Debug)]
struct Gas {
    space: u8,
    addr: u64,
}

impl Gas {
    fn parse(b: &[u8]) -> Option<Gas> {
        let addr = u64::from_le_bytes(b[4..12].try_into().ok()?);
        (addr != 0).then_some(Gas { space: b[0], addr })
    }
    fn write8(&self, v: u8) {
        match self.space {
            1 => unsafe { Port::<u8>::new(self.addr as u16).write(v) },
            0 => unsafe { core::ptr::write_volatile(map_mmio(self.addr, 1) as *mut u8, v) },
            _ => {}
        }
    }
}

#[derive(Debug)]
pub struct Acpi {
    pm1a_cnt: u16,
    pm1b_cnt: u16,
    /// PM1 event blocks (status, then enable) and their length.
    pm1a_evt: u16,
    pm1b_evt: u16,
    pm1_evt_len: u8,
    /// General-purpose event blocks (status, then enable): (port, length).
    gpe: [(u16, u8); 2],
    smi_cmd: u16,
    acpi_enable: u8,
    sleep_control: Option<Gas>,
    reset: Option<(Gas, u8)>,
    /// S5 sleep types (a, b), if the DSDT had them.
    s5: Option<(u8, u8)>,
}

/// Read `len` bytes of physical memory (ACPI tables may live anywhere).
fn phys(addr: u64, len: usize) -> Vec<u8> {
    let v = map_mmio(addr, len as u64);
    let mut out = alloc::vec![0u8; len];
    for (i, b) in out.iter_mut().enumerate() {
        *b = unsafe { core::ptr::read_volatile((v + i as u64) as *const u8) };
    }
    out
}

fn table(addr: u64) -> Option<Vec<u8>> {
    let hdr = phys(addr, 36);
    let len = u32::from_le_bytes(hdr[4..8].try_into().ok()?) as usize;
    if !(36..=4 * 1024 * 1024).contains(&len) {
        return None;
    }
    Some(phys(addr, len))
}

/// Find `_S5_` in AML and read the first two values of its package.
fn find_s5(aml: &[u8]) -> Option<(u8, u8)> {
    let mut i = 0;
    while i + 4 < aml.len() {
        if &aml[i..i + 4] == b"_S5_" && i + 5 < aml.len() && aml[i + 4] == 0x12 {
            let mut p = i + 5;
            // PkgLength: the top two bits of the first byte say how many bytes follow.
            p += 1 + (aml[p] >> 6) as usize;
            p += 1; // NumElements
            let mut vals = [0u8; 2];
            for v in vals.iter_mut() {
                match *aml.get(p)? {
                    0x0A => {
                        *v = *aml.get(p + 1)?; // BytePrefix
                        p += 2;
                    }
                    0x0B => {
                        *v = *aml.get(p + 1)?; // WordPrefix (low byte)
                        p += 3;
                    }
                    0x0C => {
                        *v = *aml.get(p + 1)?; // DWordPrefix (low byte)
                        p += 5;
                    }
                    0x0E => {
                        *v = *aml.get(p + 1)?; // QWordPrefix (low byte)
                        p += 9;
                    }
                    0x00 => {
                        *v = 0; // Zero
                        p += 1;
                    }
                    0x01 => {
                        *v = 1; // One
                        p += 1;
                    }
                    0xFF => {
                        *v = 0xFF; // Ones
                        p += 1;
                    }
                    b => {
                        *v = b;
                        p += 1;
                    }
                }
            }
            return Some((vals[0], vals[1]));
        }
        i += 1;
    }
    None
}

pub fn init(rsdp_phys: u64) -> Option<Acpi> {
    let rsdp = phys(rsdp_phys, 36);
    if &rsdp[0..8] != b"RSD PTR " {
        kprintln!("power: no ACPI RSDP");
        return None;
    }
    let (root, entry) = if rsdp[15] >= 2 && u64::from_le_bytes(rsdp[24..32].try_into().ok()?) != 0 {
        (u64::from_le_bytes(rsdp[24..32].try_into().ok()?), 8)
    } else {
        (u32::from_le_bytes(rsdp[16..20].try_into().ok()?) as u64, 4)
    };
    let root = table(root)?;
    let mut fadt = None;
    let mut ssdts = Vec::new();
    for e in root[36..].chunks_exact(entry) {
        let a = if entry == 8 { u64::from_le_bytes(e.try_into().ok()?) } else { u32::from_le_bytes(e.try_into().ok()?) as u64 };
        let sig = phys(a, 4);
        match &sig[..] {
            b"FACP" => fadt = table(a),
            b"SSDT" => ssdts.push(a),
            _ => {}
        }
    }
    let f = fadt?;
    let get32 = |o: usize| f.get(o..o + 4).map(|b| u32::from_le_bytes(b.try_into().unwrap())).unwrap_or(0);
    let get64 = |o: usize| f.get(o..o + 8).map(|b| u64::from_le_bytes(b.try_into().unwrap())).unwrap_or(0);
    let flags = get32(112);
    let dsdt = if get64(140) != 0 { get64(140) } else { get32(40) as u64 };
    let mut s5 = table(dsdt).and_then(|t| find_s5(&t));
    if s5.is_none() {
        s5 = ssdts.iter().find_map(|&a| table(a).and_then(|t| find_s5(&t)));
    }
    let mut pm1a = get32(64) as u16;
    if pm1a == 0 && f.len() >= 184 {
        pm1a = Gas::parse(&f[172..184]).map(|g| g.addr as u16).unwrap_or(0);
    }
    let reset = if flags & (1 << 10) != 0 && f.len() >= 129 { Gas::parse(&f[116..128]).map(|g| (g, f[128])) } else { None };
    let sleep_control = if flags & (1 << 20) != 0 && f.len() >= 256 { Gas::parse(&f[244..256]) } else { None };
    let a = Acpi {
        pm1a_cnt: pm1a,
        pm1b_cnt: get32(68) as u16,
        pm1a_evt: get32(56) as u16,
        pm1b_evt: get32(60) as u16,
        pm1_evt_len: f.get(88).copied().unwrap_or(0),
        gpe: [(get32(80) as u16, f.get(92).copied().unwrap_or(0)), (get32(84) as u16, f.get(93).copied().unwrap_or(0))],
        smi_cmd: get32(48) as u16,
        acpi_enable: f.get(52).copied().unwrap_or(0),
        sleep_control,
        reset,
        s5,
    };
    kprintln!(
        "power: ACPI PM1a {:#x}, S5 {:?}, reset register {}, hardware-reduced {}",
        a.pm1a_cnt,
        a.s5,
        a.reset.is_some(),
        a.sleep_control.is_some()
    );
    Some(a)
}

/// Turn the computer off. Returns only if that didn't work.
pub fn shutdown(acpi: Option<&Acpi>) {
    kprintln!("power: shutting down");
    x86_64::instructions::interrupts::disable();
    let Some(a) = acpi else { return };
    let Some((typ_a, typ_b)) = a.s5 else { return };
    if let Some(sc) = a.sleep_control {
        // Hardware-reduced ACPI: SLP_TYP in bits 2-4, SLP_EN is bit 5.
        sc.write8((typ_a & 7) << 2 | 1 << 5);
    } else if a.pm1a_cnt != 0 {
        unsafe {
            let mut cnt = Port::<u16>::new(a.pm1a_cnt);
            // Put the machine in ACPI mode first if the firmware hasn't.
            if cnt.read() & 1 == 0 && a.smi_cmd != 0 && a.acpi_enable != 0 {
                Port::<u8>::new(a.smi_cmd).write(a.acpi_enable);
                for _ in 0..1_000_000 {
                    if cnt.read() & 1 != 0 {
                        break;
                    }
                }
            }
            quiet_wake_events(a);
            // Like Linux: keep the other bits (ACPI mode!), write the sleep type, then set
            // SLP_EN in a second write.
            core::arch::asm!("wbinvd", options(nostack));
            let keep = |v: u16| v & !(7 << 10 | 1 << 13);
            let a_val = keep(cnt.read()) | (typ_a as u16 & 7) << 10;
            let mut cnt_b = Port::<u16>::new(a.pm1b_cnt);
            let b_val = if a.pm1b_cnt != 0 { keep(cnt_b.read()) | (typ_b as u16 & 7) << 10 } else { 0 };
            cnt.write(a_val);
            if a.pm1b_cnt != 0 {
                cnt_b.write(b_val);
            }
            cnt.write(a_val | 1 << 13);
            if a.pm1b_cnt != 0 {
                cnt_b.write(b_val | 1 << 13);
            }
        }
    }
    // Give the hardware a moment to cut power.
    for _ in 0..100_000_000u64 {
        core::hint::spin_loop();
    }
}

/// Restart the computer.
pub fn restart(acpi: Option<&Acpi>) -> ! {
    kprintln!("power: restarting");
    x86_64::instructions::interrupts::disable();
    if let Some((reg, val)) = acpi.and_then(|a| a.reset) {
        reg.write8(val);
        for _ in 0..10_000_000u64 {
            core::hint::spin_loop();
        }
    }
    // Keyboard controller "pulse reset line".
    unsafe {
        for _ in 0..100_000 {
            if Port::<u8>::new(0x64).read() & 2 == 0 {
                break;
            }
        }
        Port::<u8>::new(0x64).write(0xFE);
    }
    for _ in 0..10_000_000u64 {
        core::hint::spin_loop();
    }
    // Last resort: an empty interrupt table makes the next exception a triple fault.
    unsafe {
        let null = x86_64::structures::DescriptorTablePointer { limit: 0, base: x86_64::VirtAddr::new(0) };
        x86_64::instructions::tables::lidt(&null);
        core::arch::asm!("int3");
    }
    loop {
        x86_64::instructions::hlt();
    }
}

/// Before switching off: clear wake events that are already waiting and switch off every
/// wake source except the power button. A pending event (USB, network, RTC alarm, lid)
/// otherwise wakes the PC right back up, which looks like a restart.
unsafe fn quiet_wake_events(a: &Acpi) {
    for &(blk, len) in &a.gpe {
        if blk == 0 || len < 2 {
            continue;
        }
        let half = len as u16 / 2;
        for i in 0..half {
            Port::<u8>::new(blk + half + i).write(0); // enable: off
            Port::<u8>::new(blk + i).write(0xFF); // status: clear
        }
    }
    let half = a.pm1_evt_len as u16 / 2;
    for evt in [a.pm1a_evt, a.pm1b_evt] {
        if evt == 0 || half < 2 {
            continue;
        }
        Port::<u16>::new(evt + half).write(1 << 8); // only the power button may wake
        Port::<u16>::new(evt).write(0x8731); // clear timer, bus master, global lock, buttons, RTC, wake
    }
    kprintln!("power: wake events cleared");
}
