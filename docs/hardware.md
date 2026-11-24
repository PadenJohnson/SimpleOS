# Processor and motherboard

Solon's drivers for the processor and the motherboard come a step at a time, starting with
the first PC Solon was tried on: an AMD Ryzen 7 5800X (Zen 3, 8 cores and 16 threads) on an
ASUS ROG STRIX B550-F GAMING WIFI II.

What that board had working already: USB (both of its xHCI controllers), SATA, NVMe, and
sound (Realtek codec over Intel HD Audio).

Solon has no network on purpose (no internet, nothing to download: see the README), so the
board's Ethernet and Wi-Fi stay unused.

## Stages

1. **Know and measure (done).** Settings > This PC, and the log at start:
   - the processor from CPUID: its name, family and model, cores and threads, and (AMD)
     the P-state clocks (MSRs 0xC0010064 and up: FID / DID x 200 MHz);
   - on AMD Ryzen and EPYC (family 17h and newer), the way Linux's k10temp reads them: the
     control temperature Tctl (SMN 0x59800) and each chiplet's (SMN 0x59800 + 0x154 on Zen 3
     desktop parts), through the root complex's SMN index/data pair (PCI 00:00.0, registers
     0x60 and 0x64); package power from the energy counter (MSR 0xC001029B, units in MSR
     0xC0010299); the clock while busy from APERF/MPERF;
   - the motherboard, BIOS and memory modules from SMBIOS (types 0, 1, 2, 4 and 17; serial
     numbers are left alone);
   - the board's sensor chip, Nuvoton NCT6798D or NCT6799D (Linux's nct6775 driver has the
     register map): found once through the Super I/O ports (0x2e/0x4e), then only read:
     temperatures (each channel's source: SYSTIN, CPUTIN, AUXTINx, and the processor over
     SB-TSI), the 7 fan headers' RPM, and the voltages that don't depend on the board's
     resistors (CPU core, 3.3 V, 3.3 V standby, battery). Fan control stays with the board.

   Model-specific registers are read with a probe that turns "not on this processor" (a
   general-protection fault on RDMSR) into "no reading", so other machines and emulators
   simply show less.

   First try on the PC (build ac00e8e): the processor, board, BIOS 3607 and both Corsair
   16 GB DDR4-3600 modules came out right, the chip was found at 0x2e (monitor at 0x290),
   fans read 600 to 1,500 RPM, the board 22 degrees, the processor 41 degrees at 4.85 GHz
   and 51 W while starting up. Two fixes followed: chiplet temperatures past the last
   chiplet are no longer read (k10temp's limit: 8 on Zen 3 desktop; the 5800X has one, and
   registers 11 and 14 read -46 degrees), and the AUXTIN inputs are left out (two read 96
   and 83 degrees: not connected). On ASUS boards the fan headers get names (CPU fan on
   header 2, as on ASUS's other NCT6798D boards), and on the ROG STRIX B550-F the core
   voltage is doubled back (the board halves it) and +5 V and +12 V are read through the
   board's dividers (LibreHardwareMonitor's table for this board).

2. **All the cores (started).** Limine's MP request starts the other cores; Solon takes
   each over (its own GDT, TSS and double-fault stack, the IDT every core shares, interrupts
   off) and parks it in MONITOR/MWAIT on a job counter. `smp::run(n, f)` spreads `f(0..n)`
   over every core, the caller's included, and returns when all are done. Only
   self-contained work goes to the other cores (decoding, scaling); drivers, disks, the
   screen and input stay on the first core, which also takes every interrupt. Used so far
   for photo thumbnails (read one at a time on the first core, then decoded together, in
   groups whose pictures fit in about 512 MB) and the smooth scaling of the screen (the
   mirrored second monitor, lower resolutions). A processor without MONITOR/MWAIT has its
   other cores put to sleep, as does boot option "onecore". A benchmark at start logs the
   speed-up (QEMU with 4 cores: 80 ms on one core, 20 ms on four; the sample photos load in
   5.3 s instead of 7.2 s, the slowest single photo being the limit). Next: video decoding.

3. **Smaller things.** Four of the chipset controller's USB 3 ports report a connection but
   never finish link training (front-panel cabling, probably); Solon gives up on them after a
   warm reset each, which costs boot time. The board's RGB lights (an ASUS Aura USB device).
