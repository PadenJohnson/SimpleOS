//! CMOS real-time clock (wall-clock time).

use x86_64::instructions::port::Port;

fn read(reg: u8) -> u8 {
    unsafe {
        Port::<u8>::new(0x70).write(reg);
        Port::<u8>::new(0x71).read()
    }
}

fn bcd(v: u8) -> u8 {
    (v & 0x0F) + (v >> 4) * 10
}

fn write(reg: u8, v: u8) {
    unsafe {
        Port::<u8>::new(0x70).write(reg);
        Port::<u8>::new(0x71).write(v);
    }
}

/// Set the hardware clock (local time; seconds become 0). Keeps whatever format
/// (BCD or binary, 12- or 24-hour) the clock already uses, since other systems on
/// the PC (e.g. Windows) read it too.
pub fn set(y: u16, mo: u8, d: u8, h: u8, mi: u8) {
    let b = read(0x0B);
    let binary = b & 0x04 != 0;
    let h24 = b & 0x02 != 0;
    let enc = |v: u8| if binary { v } else { (v / 10) << 4 | (v % 10) };
    let hour = if h24 {
        enc(h)
    } else {
        let h12 = if h % 12 == 0 { 12 } else { h % 12 };
        enc(h12) | if h >= 12 { 0x80 } else { 0 }
    };
    write(0x0B, b | 0x80); // SET: pause updates while writing
    write(0x00, enc(0));
    write(0x02, enc(mi));
    write(0x04, hour);
    write(0x07, enc(d));
    write(0x08, enc(mo));
    write(0x09, enc((y % 100) as u8));
    write(0x0B, b & !0x80);
    kprintln!("rtc: set to {:04}-{:02}-{:02} {:02}:{:02}", y, mo, d, h, mi);
}

/// Current (hour, minute, second), 24-hour clock.
pub fn time() -> (u8, u8, u8) {
    // Wait for any in-progress update to finish so we don't read a torn value.
    for _ in 0..10_000 {
        if read(0x0A) & 0x80 == 0 {
            break;
        }
    }
    let (mut s, mut m, mut h) = (read(0x00), read(0x02), read(0x04));
    let b = read(0x0B);
    let pm = h & 0x80 != 0;
    h &= 0x7F;
    if b & 0x04 == 0 {
        s = bcd(s);
        m = bcd(m);
        h = bcd(h);
    }
    if b & 0x02 == 0 {
        // 12-hour mode
        h %= 12;
        if pm {
            h += 12;
        }
    }
    (h, m, s)
}

/// Current (year, month, day).
pub fn date() -> (u16, u8, u8) {
    for _ in 0..10_000 {
        if read(0x0A) & 0x80 == 0 {
            break;
        }
    }
    let (mut d, mut mo, mut y) = (read(0x07), read(0x08), read(0x09));
    if read(0x0B) & 0x04 == 0 {
        d = bcd(d);
        mo = bcd(mo);
        y = bcd(y);
    }
    (2000 + y as u16, mo.clamp(1, 12), d.clamp(1, 31))
}

/// Date and time packed the way FAT/exFAT timestamps store them.
pub fn fat_timestamp() -> u32 {
    let (y, mo, d) = date();
    let (h, mi, s) = time();
    ((y.saturating_sub(1980) as u32) << 25) | (mo as u32) << 21 | (d as u32) << 16 | (h as u32) << 11 | (mi as u32) << 5 | (s as u32 / 2)
}
