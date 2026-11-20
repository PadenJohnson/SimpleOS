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
