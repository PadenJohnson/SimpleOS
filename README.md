# SimpleOS
An OS for my parents family computer, an OS for my Grandma to save photos and view them. No internet, no logon, Local storage with editing capabities. supreme simplicity with enterprise safety. Desktop, usb and printer drivers with ISO download

## What it is

SimpleOS (internally "Solon") is a small operating system written from scratch in Rust for
64-bit PCs. It boots straight to a desktop: no accounts, no passwords, no network, no browser,
nothing to download.

**Working now**
- Desktop with Photos, Videos, Documents, Printers, Import, Files, Disks and Settings
- Photos: thumbnail grid, full-screen viewer, and an editor (rotate, crop with print shapes,
  brightness/contrast/saturation/warmth, Auto-fix, filters, undo, save as copy or replace
  with revert)
- USB: USB 3 (xHCI) and USB 2 (EHCI) controllers: USB drives, SD card readers (FAT12/16/32
  and exFAT), hubs, printers and phones on either; keyboards and mice on USB 3 ports.
  USB drives are only changed when you ask: Files can add copies to them (never changing or
  deleting what is already there), and Disks can erase one after two warnings.
- Storage: a "Solon Library" partition on the SimpleOS USB stick itself or in free space on
  an internal NVMe/SATA disk. Windows and other partitions are never touched.
- Files: browse Solon storage and USB drives; new folder, rename, move; Delete goes to a
  Recycle bin (restore, delete forever, empty); copy folders and files from USB drives into
  Solon and from Solon onto USB drives. Photos shows every photo in Solon storage.
- Documents: a spreadsheet app that opens and saves Excel (.xlsx) and CSV files from Solon
  storage or USB drives. Formulas (SUM, AVERAGE, IF, ROUND, SUMIF and more, recalculated as
  you type), bold, text and fill colours, number formats (money, percent, dates), column
  widths, several sheets, undo, copy/paste, fill down, and printing. Charts, pictures and
  merged cells in Excel files aren't kept when saving.
- Phones over USB: Android phones (MTP, "File transfer") and iPhones and cameras (PTP) show up
  in Files and Import. Browse and copy anything off them; Import's "Import new photos &
  videos" copies everything not imported before (big videos are copied a piece at a time).
  Phones are only read. Tested with a built-in pretend phone (boot option `testphone`);
  not yet tried with a real phone.
- Disks: every disk inside the PC and plugged in, with its partitions, sizes and free space;
  set up Solon storage; a storage health check with a safe repair; erase and format a USB
  drive (never an internal disk or the SimpleOS stick).
- Updating: an installed Solon updates itself from a USB drive (Disks, "Update from USB"):
  a newly written Solon stick, or the `distsolon-usb` files copied onto any USB drive. Only
  the system is replaced; photos, documents and settings stay. Solon says whether the USB
  copy is newer, and a power cut during the update leaves the old or the new version.
- Installing: while Solon runs from its USB stick, Disks offers "Install Solon here" on the
  internal disk (with a one-time notice at startup). It erases that disk after two warnings,
  copies Solon's boot area from the stick (so it starts on UEFI and old BIOS PCs) and
  copies everything in the stick's Solon storage. Remove the stick and restart.
- Settings: background colour, text and icon size (Normal / Large / Extra large; the whole
  desktop is scaled, which also helps on 4K screens), 12/24-hour clock, date in the top bar,
  setting the time and date, mouse speed. Saved in the library.
- Power button (top right): turn off screen, restart, shut down (ACPI, with confirmation)
- Boots from a USB stick on UEFI and older BIOS PCs; on-screen diagnostics (F12) and a crash
  screen for reporting problems

- Printing to USB printers: IPP over USB with PWG Raster (most printers since about 2015)
  and PCL (older HP printers). Letter, A4, 4x6 and 5x7; fit or fill; 1, 4 or 9 photos per
  page; colour or black & white; copies. Checked with a test printer and GhostPCL; not yet
  tried on a real printer.

**Planned:** playing videos.

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

`test.ps1` options include `-Usb` (keyboard, mouse, read-only or writable USB drives, hot-plugging), `-Internal`
(NVMe/SATA disk images), `-BootStick` / `-LegacyBios` (boot from a USB image like a real PC)
and `-Cmdline "testprinter=pcl"` (a fake printer whose jobs are captured for checking) or
`-Cmdline "testphone"` (a pretend phone with photos, a video and a document).

## Layout

- `src/`: the kernel. `main.rs` boots; `desktop.rs`, `photos.rs`, `editor.rs`, `import.rs`,
  `files.rs`, `diskmgr.rs`, `documents.rs`, `sheetui.rs`, `settingsui.rs` and `printui.rs` are
  the apps; `doc/` reads and writes spreadsheets (zip, XML, formulas, xlsx, csv); `xhci.rs`, `usb.rs`, `msc.rs`, `usbprint.rs`, `nvme.rs` and
  `ahci.rs` are drivers; `fs/` holds partition tables, FAT and exFAT; `library.rs` is the
  photo library; `print.rs` lays out pages and speaks PCL / IPP.
- `tools/`: build, image and test helpers. `docs/`: guide and screenshots. `samples/`:
  generated test photos.
