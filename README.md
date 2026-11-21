# SimpleOS
An OS for my parents family computer, an OS for my Grandma to save photos and view them. No internet, no logon, Local storage with editing capabities. supreme simplicity with enterprise safety. Desktop, usb and printer drivers with ISO download

## What it is

SimpleOS (internally "Solon") is a small operating system written from scratch in Rust for
64-bit PCs. It boots straight to a desktop: no accounts, no passwords, no network, no browser,
nothing to download.

**Working now**
- Desktop with Photos, Videos, Video Editor, Documents, Printers, Import, Files, Disks, Settings and Games
- Desktop: a taskbar along the bottom with an Applications menu and a button per open
  window; several apps open at once in windows that move, minimize and maximize
- Photos: thumbnail grid, full-screen viewer, and a Lightroom-style editor: histogram, Auto,
  Light (exposure, contrast, highlights, shadows, whites, blacks), Color (temperature, tint,
  vibrance, saturation, black & white), Effects (clarity, vignette, sharpen, fade), 12
  presets with live previews, before/after, crop with print shapes, rotate and straighten,
  undo; save as a copy, or replace with the original kept for Revert
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
- Videos: every video in Solon storage with a picture of each; plays phone and camera
  videos with sound (play/pause, drag along the bar to jump, arrow keys for 5 seconds).
  Solon's own decoders: H.264 (Baseline, Main and High profile, as phones and cameras record
  it) and AAC sound in MP4/MOV, and Motion-JPEG AVI. iPhone videos in HEVC ("High
  Efficiency") can't be played yet; the app says how to switch the iPhone to "Most
  Compatible".
- Video Editor: a simple Kdenlive-style editor. Photos, phone and camera videos (.mp4, .mov,
  .avi) and music (.wav) on the left, a preview with Play/Pause, and a timeline with Video, Titles
  and two Music tracks. Add, drag to reorder or move, drag the ends to shorten, split at the
  red line, delete, undo/redo; crossfade or dip-to-black between clips; slow zoom and pan on
  photos; titles in the middle, at the bottom or on black; volume and fades. The project
  saves itself. "Export video" makes an MP4 (H.264 picture, FLAC sound, 480p / 720p / 1080p)
  in the Videos folder, written by Solon's own encoders.
- Games (a folder in the Applications menu): Starbase Breach (an original first-person shooter:
  three station decks with keycard doors, secret walls, three weapons, four kinds of robots and
  the Warden; easy/normal/hard; automap; decks unlock as you clear them), Solitaire (draw 1 or 3),
  FreeCell, Mines (easy,
  medium, hard; right-click or "Flag" for flags), Sudoku (puzzles made on the spot, always
  with one answer; notes) and Star Pinball (an original space table with planet bumpers,
  rollover lanes, star targets, a wormhole and a ball saver; flippers on Shift or by clicking
  either half of the table). Cards move by dragging or with a single click. Best times, wins
  and the pinball high score are saved; little sound effects play through the sound card.
- Sound: Intel High Definition Audio (the sound chip in nearly every PC since about 2005),
  through the speakers, headphones and line out. Checked in QEMU; not yet on a real PC.
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
- Settings: screen resolution (any mode the PC offers; Solon restarts in it and asks "Keep
  this resolution?", going back by itself after 15 seconds; the monitor's refresh rate and name
  are shown but the refresh rate can't be changed without a graphics driver), background colour,
  text and icon size (Normal / Large / Extra large; the whole
  desktop is scaled, which also helps on 4K screens), 12/24-hour clock, date in the top bar,
  setting the time and date, mouse speed. Saved in the library.
- Power button (top right): turn off screen, restart, shut down (ACPI, with confirmation)
- Boots from a USB stick on UEFI and older BIOS PCs; on-screen diagnostics (F12) and a crash
  screen for reporting problems

- Printing to USB printers: IPP over USB with PWG Raster (most printers since about 2015)
  and PCL (older HP printers). Letter, A4, 4x6 and 5x7; fit or fill; 1, 4 or 9 photos per
  page; colour or black & white; copies. Checked with a test printer and GhostPCL; not yet
  tried on a real printer.

**Planned:** HEVC (iPhone "High Efficiency") videos; faster decoding of 1080p and 4K video.

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
| `tools\mkimg` | Host tool: builds FAT/exFAT/GPT test disks, checks partition tables, decodes print jobs, copies files out of disk images (`exfat-get`) |
| `tools\videotest` | Host tool: checks the H.264 encoder and decoder, AAC, FLAC, MP4 and AVI code against Windows' own encoders and decoders |

`test.ps1` options include `-Usb` (keyboard, mouse, read-only or writable USB drives, hot-plugging), `-Internal`
(NVMe/SATA disk images), `-BootStick` / `-LegacyBios` (boot from a USB image like a real PC)
and `-Cmdline "testprinter=pcl"` (a fake printer whose jobs are captured for checking) or
`-Cmdline "testphone"` (a pretend phone with photos, a video and a document), `-Sound` (a sound
card whose output is saved to `test-outsound.wav`; with `-Cmdline "soundtest"` it plays a tone).

## Layout

- `src/`: the kernel. `main.rs` boots; `desktop.rs`, `photos.rs`, `editor.rs`, `import.rs`,
  `files.rs`, `diskmgr.rs`, `documents.rs`, `sheetui.rs`, `movieui.rs`, `settingsui.rs` and `printui.rs` are
  the apps; `movie.rs` is the video editor's engine; `video/` holds the H.264 encoder and decoder (CAVLC and CABAC), AAC, FLAC, MP4 and AVI/WAV code (`tables.rs`: standard tables from FFmpeg); `videos.rs` is the player; `games/` holds the games;
  `doc/` reads and writes spreadsheets (zip, XML, formulas, xlsx, csv); `xhci.rs`, `ehci.rs`, `usb.rs`, `msc.rs`, `usbprint.rs`, `nvme.rs`, `hda.rs` and
  `ahci.rs` are drivers; `fs/` holds partition tables, FAT and exFAT; `library.rs` is the
  photo library; `print.rs` lays out pages and speaks PCL / IPP.
- `tools/`: build, image and test helpers. `docs/`: guide and screenshots. `samples/`:
  generated test photos.
