# SimpleOS
An OS for my parents family computer, an OS for my Grandma to save photos and view them. No internet, no logon, Local storage with editing capabities. supreme simplicity with enterprise safety. Desktop, usb and printer drivers with ISO download

## What it is

SimpleOS (internally "Solon") is a small operating system written from scratch in Rust for
64-bit PCs. It boots straight to a desktop: no accounts, no passwords, no network, no browser,
nothing to download.

**Working now**
- Desktop with Photos, Videos, Documents, Printers and Import
- Photos: thumbnail grid, full-screen viewer, and an editor (rotate, crop with print shapes,
  brightness/contrast/saturation/warmth, Auto-fix, filters, undo, save as copy or replace
  with revert)
- USB: keyboards, mice, hubs, USB drives and SD card readers (FAT12/16/32 and exFAT).
  Drives are only ever read; nothing on them is changed.
- Storage: a "Solon Library" partition on the SimpleOS USB stick itself or in free space on
  an internal NVMe/SATA disk. Windows and other partitions are never touched.
- Boots from a USB stick on UEFI and older BIOS PCs; on-screen diagnostics (F12) and a crash
  screen for reporting problems

**In progress:** printing to USB printers (IPP over USB and PCL, aimed at HP printers).
**Planned:** videos, documents, installing to the internal disk.

## Making a USB stick

See [docs/real-pc-guide.md](docs/real-pc-guide.md). In short:

```powershell
git clone https://github.com/limine-bootloader/limine.git --branch=v9.x-binary --depth=1
powershell -ExecutionPolicy Bypass -File .\tools\make-usb.ps1
```

Then write `dist\solon.img` to a stick with Rufus or balenaEtcher, turn off Secure Boot, and
boot the PC from the stick.

## Developing

Needs Rust (the nightly toolchain is picked automatically from `rust-toolchain.toml`) and
QEMU for Windows.

| Command | What it does |
|---|---|
| `.\run.ps1` | Build and run in a QEMU window with the sample photos |
| `.\test.ps1` | Build and boot headless; drives mouse/keyboard, takes screenshots, checks the log |
| `.\tools\make-usb.ps1` | Build the real-PC USB files and disk image into `dist\` |
| `.\tools\make-samples.ps1` | Regenerate the sample photos |
| `tools\mkimg` | Host tool: builds FAT/exFAT/GPT test disks, checks partition tables, decodes print jobs |

`test.ps1` options include `-Usb` (keyboard, mouse, USB drives, hot-plugging), `-Internal`
(NVMe/SATA disk images), `-BootStick` / `-LegacyBios` (boot from a USB image like a real PC)
and `-Cmdline "testprinter=pcl"` (a fake printer whose jobs are captured for checking).

## Layout

- `src/`: the kernel. `main.rs` boots; `desktop.rs`, `photos.rs`, `editor.rs`, `import.rs`
  and `printui.rs` are the apps; `xhci.rs`, `usb.rs`, `msc.rs`, `usbprint.rs`, `nvme.rs` and
  `ahci.rs` are drivers; `fs/` holds partition tables, FAT and exFAT; `library.rs` is the
  photo library; `print.rs` lays out pages and speaks PCL / IPP.
- `tools/`: build, image and test helpers. `docs/`: guide and screenshots. `samples/`:
  generated test photos.
