# SimpleOS
An OS for my parents family computer, an OS for my Grandma to save photos and view them. No internet, no logon, Local storage with editing capabities. supreme simplicity with enterprise safety. Desktop, usb and printer drivers with ISO download

## The idea

The idea of this OS is to be the most advanced while acceptable for people remembering the
past. Underneath, it uses today's hardware to the full: every processor core, its own
graphics drivers lighting a second monitor, every common photo and video format. On top, it
looks and works the way people already know computers: windows, a dock, folders, a big clock,
plain words instead of jargon, big clear text, and nothing that changes or asks for attention
by itself. New things arrive working, set up sensibly from the start (a second monitor simply
extends the desktop), with the choices in Settings for anyone who wants them.

## What it is

SimpleOS (internally "Solon") is a small operating system written from scratch in Rust for
64-bit PCs. It boots straight to a desktop: no accounts, no passwords, no network, no browser,
nothing to download.

**Working now**
- Desktop with Photos, Photo Frame, Videos, Video Editor, Documents (Writer, Spreadsheets, PDF, Text Editor),
  Paint, Calendar, Calculator, Printers, Backup, Import, Files, Disks, Settings and Games
- Desktop: a nature photo (five built in: autumn, sunset, forest, field, snow; free CC0
  photos, see assets/wallpapers/CREDITS.md), a soft colour, or one of your own photos ("Desktop
  picture" in Photos) behind a big clock whose colour adapts to the picture; a floating dock (Applications button, pinned
  apps and folders, open apps, clock, power) and a centred Applications launcher; several
  apps open at once in rounded windows that move, minimize and maximize. Smooth Noto Sans
  and Noto Serif fonts throughout
- Photos: thumbnail grid, full-screen viewer, and a Lightroom-style editor: histogram, Auto,
  Light (exposure, contrast, highlights, shadows, whites, blacks), Color (temperature, tint,
  vibrance, saturation, black & white), Effects (clarity, vignette, sharpen, fade), 12
  presets with live previews, before/after, crop with print shapes, rotate and straighten,
  undo; save as a copy, or replace with the original kept for Revert. Opens every common
  photo type: JPEG, PNG, HEIC/HEIF (iPhone), WebP, GIF (first frame), TIFF, BMP, camera RAW
  (CR2, CR3, NEF, ARW, DNG, RAF, ORF, RW2, PEF and more, via the full-size preview inside),
  Photoshop PSD, TGA, ICO and PBM/PGM/PPM; photos taken sideways are turned upright. Edited
  photos save as JPEG, or PNG when they have see-through parts. Not yet: AVIF and JPEG XL.
- USB: USB 3 (xHCI) and USB 2 (EHCI) controllers: USB drives, SD card readers (FAT12/16/32
  and exFAT), hubs, printers and phones on either; keyboards and mice on USB 3 ports.
  USB drives are only changed when you ask: Files can add copies to them (never changing or
  deleting what is already there), and Disks can erase one after two warnings.
- Storage: a "Solon Library" partition on the SimpleOS USB stick itself or in free space on
  an internal NVMe/SATA disk. Windows and other partitions are never touched.
- Files: browse Solon storage and USB drives; new folder, rename, move; Delete goes to a
  Recycle bin (restore, delete forever, empty); copy folders and files from USB drives into
  Solon and from Solon onto USB drives. Photos shows every photo in Solon storage.
- Documents (a folder in the Applications menu):
  - Writer: a word processor that opens and saves Word (.docx) files: fonts and sizes, bold,
    italic, underline, colours, highlighting, headings, alignment, bullets and numbering,
    page breaks, pictures, tables (Tab moves between cells), undo, printing and Save as
    PDF. Text boxes and drawings made in Word aren't shown yet.
  - PDF: view any PDF, add text, draw, highlight, sign with the mouse, turn/move/delete
    pages, add pages from other PDFs or photos, save and print; "PDF from photos" makes new
    PDFs. Opens protected PDFs that have no open password, and scanned (fax-style) pages.
    JPEG 2000 and JBIG2 pictures in PDFs show as grey boxes for now.
  - Text Editor: plain text and code files, keeping their encoding and line endings; wrap,
    find and replace, bigger text, printing.
  - Spelling, for every app where you type text: Writer, the Text Editor (on for .txt and
    .md files; the Spelling button turns it on or off), spreadsheet cells, calendar events
    and video titles. Mistakes get a red wavy line; right-click one (or press F7, Shift+F10
    or the Menu key) for suggestions (typos and sound-alike spellings: "recieve" -> receive,
    "partee" -> party, "fone" -> phone), Add to dictionary or Ignore. F7 also jumps to the next
    mistake. English (US) or English (UK) in Settings, or off. Added words are kept in
    Solon storage (Dictionary.txt) and backed up. The dictionary is SCOWL's (about 92,000
    words, names and abbreviations).
  - Spreadsheets: opens and saves Excel (.xlsx) and CSV files from Solon
    storage or USB drives. Formulas (SUM, AVERAGE, IF, ROUND, SUMIF and more, recalculated as
    you type), bold, text and fill colours, number formats (money, percent, dates), column
    widths, several sheets, undo, copy/paste, fill down, and printing. Charts, pictures and
    merged cells in Excel files aren't kept when saving.
- Videos: every video in Solon storage with a picture of each; plays phone, camera, computer,
  DVD/TV and web videos with sound (play/pause, drag along the bar to jump, arrow keys for 5
  seconds). Files: MP4/MOV/3GP, MKV, WebM, AVI, MPEG-TS (.ts/.m2ts/.mts) and MPEG-PS
  (.mpg/.vob). Pictures: H.264, HEVC/H.265 (iPhone "High Efficiency", 8 and 10-bit), MPEG-1/2,
  MPEG-4 Part 2 (Xvid, DivX 4/5), DivX 3 (Microsoft MPEG-4 v3), H.263, VP8, VP9 (8, 10 and
  12-bit) and Motion JPEG. Sound: AAC, MP3, MP2, AC-3 and E-AC-3 (Dolby Digital and Dolby
  Digital Plus), DTS, AMR and AMR-WB (old phones' 3GP videos), Vorbis, Opus and PCM; surround
  is mixed to stereo. All with Solon's own decoders, checked against FFmpeg. DTS-HD and 7.1
  E-AC-3 play their main 5.1 part. Not yet: WMV/VC-1, AV1, RealVideo, Theora.
  A classic to try it with: Steamboat Willie (1928), public domain in the US since 2024
  (still under copyright in some countries, such as Germany; Mickey Mouse is a Disney
  trademark). `tools/get-steamboat-willie.sh` downloads the 1080p VP9 version from Wikimedia
  Commons and can copy it into a stick's Solon storage (Videos folder). Solon's decoder plays
  it frame for frame the same as FFmpeg.
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
  with one answer; notes) and Neon Grill Pinball (an original 1980s-diner table: a 3D opening of
  the whole machine lighting up in a dark arcade before the camera swoops down onto the
  playfield; shaded burger, donut and pie bumpers, E-A-T lanes, M-E-N-U drop targets, a fries
  spinner, a see-through milkshake ramp, the kitchen window, glowing lamps, food orders to fill
  and serve, a two-ball lunch rush, a dot-matrix score display and a ball saver; flippers on
  Shift or by clicking either half of the table). Cards move by dragging or with a single
  click. Best times, wins and the pinball high score are saved; little sound effects play
  through the sound card.
- Sound: Intel High Definition Audio (the sound chip in nearly every PC since about 2005),
  through the speakers, headphones and line out. Checked in QEMU; not yet on a real PC.
- Phones over USB: Android phones (MTP, "File transfer") and iPhones and cameras (PTP) show up
  in Files and Import. Browse and copy anything off them; Import's "Import new photos &
  videos" copies everything not imported before (big videos are copied a piece at a time).
  Phones are only read. When a phone is connected, Solon offers to continue working on its
  newest document (Word, Excel, CSV, PDF, text or Markdown, in its Documents or Download
  folders): Open shows it in the app for that kind of file, and saving keeps a copy in Solon's
  Documents. Tested with a built-in pretend phone (boot option `testphone`);
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
- Settings, in sections with a sidebar: Appearance (background, text and icon size: Normal /
  Large / Extra large, the whole desktop scaled, which also helps on 4K screens), Date & time
  (12/24-hour clock, the date next to the clock, and setting the date on a calendar and the
  time from drop-down lists), Display (screen resolution from a drop-down list, changed at once
  without a restart: in virtual machines (QEMU, VirtualBox) the graphics switch modes; on
  other PCs lower resolutions are drawn at their size and scaled smoothly to fill the screen,
  as a monitor does; only one bigger than the screen's start-up mode needs a restart, after
  which Solon asks "Keep this resolution?" and goes back by itself after 15 seconds; the
  monitor's name and refresh rate; with two monitors, Screens: a picture of how they stand,
  Extend (one desktop across both, the default), Mirror or Main screen only, which one is the
  main screen (the dock, the clock, messages and new windows), where the second one stands
  (left, right, above or below) and how they line up; the screensaver), Mouse (speed),
  Spelling (language, and the words you added, with Remove), This PC and About. Saved in the
  library.
  Right-click the desktop for quick ways to the background, display, screensaver, date and
  time, text size and spelling settings.
- Graphics: works on every Intel, AMD and NVIDIA card through the firmware's screen; Solon
  names the card (Settings > About), uses write-combining and sends only the changed parts of
  the screen to the card. Native drivers come a step at a time: see
  [docs/graphics.md](docs/graphics.md). The NVIDIA driver (GeForce RTX 20 series and newer)
  reads the card, its display engine and which connectors have monitors, and on RTX 30
  series cards lights a second DisplayPort monitor when Solon starts and spreads the desktop
  across both: the pointer and windows move between them, windows maximize on the screen
  they're on, and each screen has its own wallpaper (Settings > Display > Screens).
- Every processor core is used: photo thumbnails are decoded together and the screen is
  scaled on all cores (see [docs/hardware.md](docs/hardware.md)).
- This PC (Settings): the processor (name, cores and threads; on AMD Ryzen its clock,
  temperature and package power, live), the motherboard and BIOS and each memory module (from
  the firmware's SMBIOS tables), and the board's sensors on Nuvoton NCT6798D/NCT6799D chips
  (fans, temperatures, voltages). See [docs/hardware.md](docs/hardware.md).
- Diagnostics (F12): the log; S saves it (and what the NVIDIA driver read) to the Solon stick.
- Power button (top right): turn off screen, restart, shut down (ACPI, with confirmation)
- Boots from a USB stick on UEFI and older BIOS PCs; on-screen diagnostics (F12) and a crash
  screen for reporting problems

- Printing to USB printers in five languages, chosen per printer: driverless IPP over USB
  with PWG raster or AirPrint (URF) raster (most printers since about 2015, every brand),
  PCL (HP and compatibles), PCL 6 and PostScript (Brother, Kyocera, Lexmark, Ricoh, Xerox,
  Samsung, Konica Minolta, Canon imageCLASS lasers). Letter, A4, 4x6 and 5x7; fit or fill;
  1, 4 or 9 photos per page; colour or black & white; copies. The Printers app shows each
  printer, how Solon talks to it and its status, prints a test page, and explains printers
  that only speak a closed format (HP host-based, Canon CAPT, Samsung GDI, older Epson and
  Canon inkjets). Checked with test printers and decoders (tools/printtest); not yet tried
  on a real printer.
- Calendar: month view with today marked, US holidays, and events (times, every
  week/month/year for birthdays) kept in Solon storage; today's appear under the desktop
  clock, and clicking the dock clock opens the Calendar.
- Paint: a drawing program like MS Paint with a clean Mac-style look: pencil, brush, spray,
  eraser, fill, colour picker, text, lines, rectangles, rounded rectangles and ellipses
  (outline, filled or both), select and move, copy/cut/paste, crop, rotate, flip, resize,
  zoom, undo/redo; opens every photo type above, saves PNG and JPEG, and prints.
- Calculator: big buttons, x and / before + and -, percent, square root, squares, 1/x,
  a history of sums, and the keyboard.
- Photo Frame and screensaver: your photos full screen, one after another, fading into each
  other over a blurred copy of the photo, with the time and date (photos taken sideways are
  turned upright). Start it from the Photo Frame app, or let it start by itself when the
  computer isn't used for a while (1 minute to 1 hour; photos or just a clock; 5 seconds
  to 1 minute per photo; mixed up or in order). Move the mouse or press a key to stop.
- Backup: one button copies everything in Solon storage to a USB drive, into a "Solon Backup"
  folder that opens on any computer. Only new and changed files are copied; changed files are
  added beside the old copy with the date in the name, so older versions stay. Every copy is
  read back and checked, and nothing already on the drive is changed or deleted. Restore
  copies a chosen backup back into Solon without replacing anything (a changed file comes
  back as "Name (restored)"). The desktop reminds you when the last backup is over 30 days
  old, and when a backup drive is plugged in.

**Planned:** faster decoding of 1080p and 4K video.

## Making a USB stick

Download `solon.img` from the [latest release](https://github.com/PadenJohnson/SimpleOS/releases/tag/latest)
(rebuilt and boot-tested before every push), or build it yourself. See
[docs/real-pc-guide.md](docs/real-pc-guide.md). In short:

```powershell
git clone https://github.com/limine-bootloader/limine.git --branch=v9.x-binary --depth=1
powershell -ExecutionPolicy Bypass -File .\tools\make-usb.ps1
```

Then write `dist\solon.img` to a stick with Rufus or balenaEtcher and boot the PC from the
stick. Secure Boot can stay on: the first time, approve Solon once in the blue shim screen
(see docs/real-pc-guide.md and secureboot/README.md).

## Developing

Needs Rust (the nightly toolchain is picked automatically from `rust-toolchain.toml`) and
QEMU for Windows. On Linux (including a Raspberry Pi, where QEMU emulates the PC in software, so
booting takes about 25 seconds) use `./run.sh` and `./test.sh`, which need `qemu-system-x86`,
`ovmf` and `python3`.

| Command | What it does |
|---|---|
| `.\run.ps1` | Build and run in a QEMU window with the sample photos |
| `.\test.ps1` | Build and boot headless; drives mouse/keyboard, takes screenshots, checks the log |
| `./run.sh`, `./test.sh` | The same on Linux (`./test.sh --help`; same options as `test.ps1`, plus `--wait-for "solon: desktop ready"` to go on as soon as a log line appears) |
| `.\tools\make-usb.ps1` | Build the real-PC USB files and disk image into `dist\` |
| `tools/make-usb.sh` | The same on Linux |
| `tools/publish-usb.sh` | Upload `dist/` to the GitHub release `latest` (run by the pre-push hook) |
| `.\tools\make-samples.ps1` | Regenerate the sample photos |
| `tools\mkimg` | Host tool: builds FAT/exFAT/GPT test disks, checks partition tables, decodes print jobs, copies files out of disk images (`exfat-get`) |
| `tools\videotest` | Host tool: checks the H.264 encoder and decoder, AAC, FLAC, MP4 and AVI code against Windows' own encoders and decoders |
| `tools\mediatest` | Host tool: checks the video player's decoders frame by frame and sample by sample against FFmpeg |

`test.ps1` options include `-Usb` (keyboard, mouse, read-only or writable USB drives, hot-plugging), `-Internal`
(NVMe/SATA disk images), `-BootStick` / `-LegacyBios` (boot from a USB image like a real PC)
and `-Cmdline "testprinter=pcl"` (or ipp, urf, pclmono, pclxl, ps) (a fake printer whose jobs are captured for checking) or
`-Cmdline "testphone"` (a pretend phone with photos, a video and a document), `-Cmdline "novbe"`
(leave QEMU's graphics alone, so resolution changes are scaled as on a real PC), `-Sound` (a sound
card whose output is saved to `test-out\sound.wav`; with `-Cmdline "soundtest"` it plays a tone). `test.sh` takes the same
options, written either way: `--usb kbd mouse` or `-Usb kbd mouse`.

Pushing `main` runs `.githooks/pre-push` (turn it on once per clone with
`git config core.hooksPath .githooks`; needs Linux, QEMU and a logged-in `gh`): it rebuilds the USB
image, boot-tests it with UEFI and old BIOS, and publishes it as the release `latest`.
`SKIP_USB_IMAGE=1 git push` skips it.

## Layout

- `src/`: the kernel. `main.rs` boots; `desktop.rs`, `photos.rs`, `editor.rs`, `import.rs`,
  `files.rs`, `diskmgr.rs`, `documents.rs`, `sheetui.rs`, `movieui.rs`, `settingsui.rs` and `printui.rs` are
  the apps; `movie.rs` is the video editor's engine; `video/` holds the H.264 encoder and decoder (CAVLC and CABAC), AAC, FLAC, MP4 and AVI/WAV code (`tables.rs`: standard tables from FFmpeg); `videos.rs` is the player and `media/` its engine (containers and the MPEG-2, MPEG-4, DivX 3, HEVC, VP8, VP9, MP3, AC-3/E-AC-3, DTS, AMR, Vorbis and Opus decoders); `games/` holds the games; `writer/` is Writer (with .docx), `pdf/` reads, draws and writes PDFs (`pdfui.rs` is the PDF app), `gpu.rs` recognises graphics cards, `screen.rs` puts the desktop on the screen (only what changed), `display.rs` handles resolutions; `textedit.rs` the Text Editor and `docshell.rs` what those three share; `spell.rs` is spelling for every app (assets/spell/: SCOWL's word lists, made with tools/spell/mkdict.py, see assets/spell/SCOWL-Copyright.txt); `font.rs` and `raster.rs` draw text (fonts/: Noto Sans and Noto Serif, SIL Open Font License, see fonts/OFL.txt);
  `doc/` reads and writes spreadsheets (zip, XML, formulas, xlsx, csv); `xhci.rs`, `ehci.rs`, `usb.rs`, `msc.rs`, `usbprint.rs`, `nvme.rs`, `hda.rs` and
  `ahci.rs` are drivers; `fs/` holds partition tables, FAT and exFAT; `library.rs` is the
  photo library; `image.rs` and `imgfmt/` read photo files (HEIC, WebP, GIF, TIFF, RAW previews, PSD...); `print.rs` lays out pages and speaks PCL / IPP.
- `tools/`: build, image and test helpers. `docs/`: guide and screenshots. `samples/`:
  generated test photos.
