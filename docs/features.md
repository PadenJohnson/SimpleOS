# What SimpleOS does

Everything SimpleOS (Solon) can do today, app by app. The [README](../README.md) has the short
version and screenshots; [graphics.md](graphics.md) and [hardware.md](hardware.md) follow the
drivers being written for real PCs.

Things marked *waiting for a try* work in the test machine (QEMU) and are written for real
hardware, but haven't been confirmed on a real PC yet.

## Desktop

- A nature photo (five built in: autumn, sunset, forest, field, snow; free CC0 photos, see
  [assets/wallpapers/CREDITS.md](../assets/wallpapers/CREDITS.md)), a soft colour, or one of
  your own photos ("Desktop picture" in Photos) behind a big clock whose colour adapts to the
  picture, with today's date, holidays and calendar events under it.
- A floating dock: the Applications button, pinned apps and folders, open apps, the clock and
  the power button. With two monitors every screen has one; right-click it for its options:
  the taskbar on every screen, the date, the 24-hour clock, and the way to the sound and
  screen settings.
- A centred Applications launcher with folders (Documents, Games).
- Several apps open at once in rounded windows that move, minimize and maximize. Smooth Noto
  Sans and Noto Serif fonts throughout.
- Right-click the desktop for quick ways to the background, screens, sound, screensaver, date
  and time, text size and spelling settings.
- The power button: turn off the screen, restart, shut down (ACPI, with a confirmation).

## Photos

- Thumbnail grid, full-screen viewer, and a Lightroom-style editor: histogram, Auto, Light
  (exposure, contrast, highlights, shadows, whites, blacks), Color (temperature, tint,
  vibrance, saturation, black & white), Effects (clarity, vignette, sharpen, fade), 12 presets
  with live previews, before/after, crop with print shapes, rotate and straighten, undo; save
  as a copy, or replace with the original kept for Revert.
- Opens every common photo type: JPEG, PNG, HEIC/HEIF (iPhone), WebP, GIF (first frame), TIFF,
  BMP, camera RAW (CR2, CR3, NEF, ARW, DNG, RAF, ORF, RW2, PEF and more, via the full-size
  preview inside), Photoshop PSD, TGA, ICO and PBM/PGM/PPM. Photos taken sideways are turned
  upright. Edited photos save as JPEG, or PNG when they have see-through parts. Not yet: AVIF
  and JPEG XL.
- Thumbnails are decoded on every processor core at once.

## Photo Frame and screensaver

Your photos full screen, one after another, fading into each other over a blurred copy of the
photo, with the time and date. Start it from the Photo Frame app, or let it start by itself
when the computer isn't used for a while (1 minute to 1 hour; photos or just a clock; 5
seconds to 1 minute per photo; mixed up or in order). Move the mouse or press a key to stop.

## Documents

A folder in the Applications menu.

- **Writer**: a word processor that opens and saves Word (.docx) files: fonts and sizes, bold,
  italic, underline, colours, highlighting, headings, alignment, bullets and numbering, page
  breaks, pictures, tables (Tab moves between cells), undo, printing and Save as PDF. Text
  boxes and drawings made in Word aren't shown yet.
- **Spreadsheets**: opens and saves Excel (.xlsx) and CSV files from Solon storage or USB
  drives. Formulas (SUM, AVERAGE, IF, ROUND, SUMIF and more, recalculated as you type), bold,
  text and fill colours, number formats (money, percent, dates), column widths, several
  sheets, undo, copy/paste, fill down, and printing. Charts, pictures and merged cells in Excel
  files aren't kept when saving.
- **PDF**: view any PDF, add text, draw, highlight, sign with the mouse, turn/move/delete
  pages, add pages from other PDFs or photos, save and print; "PDF from photos" makes new PDFs.
  Opens protected PDFs that have no open password, and scanned (fax-style) pages. JPEG 2000
  and JBIG2 pictures in PDFs show as grey boxes for now.
- **Text Editor**: plain text and code files, keeping their encoding and line endings; wrap,
  find and replace, bigger text, printing.
- **Spelling**, in every app where you type: Writer, the Text Editor (on for .txt and .md
  files; the Spelling button turns it on or off), spreadsheet cells, calendar events and video
  titles. Mistakes get a red wavy line; right-click one (or press F7, Shift+F10 or the Menu
  key) for suggestions (typos and sound-alike spellings: "recieve" -> receive, "partee" ->
  party, "fone" -> phone), Add to dictionary or Ignore. F7 also jumps to the next mistake.
  English (US) or English (UK) in Settings, or off. Added words are kept in Solon storage
  (Dictionary.txt) and backed up. The dictionary is SCOWL's (about 92,000 words, names and
  abbreviations).

## Videos

- Every video in Solon storage, on USB drives and on phones, with a picture of each; plays
  phone, camera, computer, DVD/TV and web videos with sound: click the picture to pause or play
  (a big sign shows which; double-click for full screen), drag along the bar to jump, arrow
  keys for 10 seconds, volume.
- Smooth playback: while a video plays only its picture is drawn again and sent to the screen,
  each picture is converted and scaled on every processor core, and after a jump the sound
  waits for the picture so they start together. How it's keeping up goes to the log every 10
  seconds.
- Files: MP4/MOV/3GP, MKV, WebM, AVI, MPEG-TS (.ts/.m2ts/.mts) and MPEG-PS (.mpg/.vob).
- Pictures: H.264, HEVC/H.265 (iPhone "High Efficiency", 8 and 10-bit), MPEG-1/2, MPEG-4 Part 2
  (Xvid, DivX 4/5), DivX 3 (Microsoft MPEG-4 v3), H.263, VP8, VP9 (8, 10 and 12-bit) and
  Motion JPEG.
- Sound: AAC, MP3, MP2, AC-3 and E-AC-3 (Dolby Digital and Dolby Digital Plus), DTS, AMR and
  AMR-WB (old phones' 3GP videos), Vorbis, Opus and PCM; surround is mixed to stereo. DTS-HD
  and 7.1 E-AC-3 play their main 5.1 part.
- All with Solon's own decoders, checked against FFmpeg frame by frame and sample by sample
  (tools/mediatest). Not yet: WMV/VC-1, AV1, RealVideo, Theora.
- A classic to try it with: Steamboat Willie (1928), public domain in the US since 2024 (still
  under copyright in some countries, such as Germany; Mickey Mouse is a Disney trademark).
  `tools/get-steamboat-willie.sh` downloads the 1080p VP9 version from Wikimedia Commons and
  can copy it into a stick's Solon storage (Videos folder). Solon's decoder plays it frame for
  frame the same as FFmpeg.

## Video Editor

A simple Kdenlive-style editor. Photos, phone and camera videos (.mp4, .mov, .avi) and music
(.wav) on the left, a preview with Play/Pause, and a timeline with Video, Titles and two Music
tracks. Add, drag to reorder or move, drag the ends to shorten, split at the red line, delete,
undo/redo; crossfade or dip-to-black between clips; slow zoom and pan on photos; titles in the
middle, at the bottom or on black; volume and fades. The project saves itself. "Export video"
makes an MP4 (H.264 picture, FLAC sound, 480p / 720p / 1080p) in the Videos folder, written by
Solon's own encoders.

## Games

A folder in the Applications menu. Best times, wins and the pinball high score are saved;
little sound effects play through the sound card. Cards move by dragging or with a single
click.

- **Neon Grill Pinball**: an original 1980s-diner table. A 3D opening shows the whole machine
  lighting up in a dark arcade before the camera swoops down onto the playfield; shaded burger,
  donut and pie bumpers, E-A-T lanes, M-E-N-U drop targets, a fries spinner, a see-through
  milkshake ramp, the kitchen window, glowing lamps, food orders to fill and serve, a two-ball
  lunch rush, a dot-matrix score display and a ball saver. Flippers on Shift or by clicking
  either half of the table.
- **Starbase Breach**: an original first-person shooter: three station decks with keycard
  doors, secret walls, three weapons, four kinds of robots and the Warden; easy/normal/hard;
  automap; decks unlock as you clear them.
- **Solitaire** (draw 1 or 3), **FreeCell**, **Mines** (easy, medium, hard; right-click or
  "Flag" for flags) and **Sudoku** (puzzles made on the spot, always with one answer; notes).

## Calendar, Paint and Calculator

- **Calendar**: month view with today marked, US holidays, and events (times, every
  week/month/year for birthdays) kept in Solon storage; today's appear under the desktop clock,
  and clicking the dock clock opens the Calendar.
- **Paint**: a drawing program like MS Paint with a clean Mac-style look: pencil, brush, spray,
  eraser, fill, colour picker, text, lines, rectangles, rounded rectangles and ellipses
  (outline, filled or both), select and move, copy/cut/paste, crop, rotate, flip, resize, zoom,
  undo/redo; opens every photo type above, saves PNG and JPEG, and prints.
- **Calculator**: big buttons, x and / before + and -, percent, square root, squares, 1/x, a
  history of sums, and the keyboard.

## Storage, files and backup

- **Storage**: a "Solon Library" partition on the SimpleOS USB stick itself or in free space
  on an internal NVMe/SATA disk. Windows and other partitions are never touched.
- **Files**: browse Solon storage and USB drives; new folder, rename, move; Delete goes to a
  Recycle bin (restore, delete forever, empty); copy folders and files from USB drives into
  Solon and from Solon onto USB drives.
- **Disks**: every disk inside the PC and plugged in, with its partitions, sizes and free
  space; set up Solon storage; a storage health check with a safe repair; erase and format a
  USB drive (never an internal disk or the SimpleOS stick).
- **Backup**: one button copies everything in Solon storage to a USB drive, into a "Solon
  Backup" folder that opens on any computer. Only new and changed files are copied; changed
  files are added beside the old copy with the date in the name, so older versions stay.
  Every copy is read back and checked, and nothing already on the drive is changed or deleted.
  Restore copies a chosen backup back into Solon without replacing anything (a changed file
  comes back as "Name (restored)"). The desktop reminds you when the last backup is over 30
  days old, and when a backup drive is plugged in.

## USB, phones and printers

- **USB**: USB 3 (xHCI) and USB 2 (EHCI) controllers: USB drives, SD card readers (FAT12/16/32
  and exFAT), hubs, printers and phones on either; keyboards and mice on USB 3 ports. USB
  drives are only changed when you ask: Files can add copies to them (never changing or
  deleting what is already there), and Disks can erase one after two warnings.
- **Phones**: Android phones (MTP, "File transfer"), iPhones and cameras (PTP) show up in Files
  and Import. Browse and copy anything off them; Import's "Import new photos & videos" copies
  everything not imported before (big videos are copied a piece at a time). Phones are only
  read. When a phone is connected, Solon offers to continue working on its newest document
  (Word, Excel, CSV, PDF, text or Markdown, in its Documents or Download folders). Tested with a
  built-in pretend phone (boot option `testphone`); *waiting for a try* with a real phone.
- **Printing** to USB printers in five languages, chosen per printer: driverless IPP over USB
  with PWG raster or AirPrint (URF) raster (most printers since about 2015, every brand), PCL
  (HP and compatibles), PCL 6 and PostScript (Brother, Kyocera, Lexmark, Ricoh, Xerox, Samsung,
  Konica Minolta, Canon imageCLASS lasers). Letter, A4, 4x6 and 5x7; fit or fill; 1, 4 or 9
  photos per page; colour or black & white; copies. The Printers app shows each printer, how
  Solon talks to it and its status, prints a test page, and explains printers that only speak
  a closed format (HP host-based, Canon CAPT, Samsung GDI, older Epson and Canon inkjets).
  Checked with test printers and decoders (tools/printtest); *waiting for a try* on a real
  printer.

## Sound

- Intel High Definition Audio (the sound chip in nearly every PC since about 2005): the
  computer's jacks (speakers, headphones, optical; each named by its colour and place, and
  whether something is plugged in), and the monitors on an NVIDIA RTX graphics card over
  DisplayPort (their speakers or headphone jack). Works on a Ryzen PC's Realtek chip; AMD and
  NVIDIA sound controllers are told to snoop the processor's caches as Linux does them.
- USB sound devices (USB Audio Class 1 and 2: headsets, USB speakers and headphones, headphone
  DACs) on USB 3 ports. Checked with QEMU's USB sound device; *waiting for a try* with a real
  one.
- Settings > Sound shows what's connected (jacks with something plugged in, monitors, USB sound
  devices; "Show every output" for the rest) and chooses where sound plays: Automatic (a USB
  sound device, else headphones or speakers you plug in, else a monitor's speakers), one of
  them, or every jack at once, each with a Test button; and how loud.

## Screens and graphics

- Works on every Intel, AMD and NVIDIA card through the firmware's screen; Solon names the card
  (Settings > About), uses write-combining and sends only the changed parts of the screen.
- Screen resolution from a drop-down list, changed at once without a restart: in virtual
  machines the graphics switch modes; on other PCs lower resolutions are drawn at their size
  and scaled smoothly to fill the screen, as a monitor does. Only one bigger than the screen's
  start-up mode needs a restart, after which Solon asks "Keep this resolution?" and goes back
  by itself after 15 seconds.
- The NVIDIA driver (GeForce RTX 20 series and newer) reads the card, its display engine and
  which connectors have monitors, and on RTX 30 series cards lights a second DisplayPort
  monitor when Solon starts. The desktop spreads across both: the pointer and windows move
  between them, windows maximize on the screen they're on, and each screen has its own
  wallpaper. Settings > Display > Screens: Extend (the default), Mirror or Main screen only;
  which screen is the main one; where the second one stands and how they line up. See
  [graphics.md](graphics.md).

## Processor and motherboard

- Every processor core is used: photo thumbnails are decoded together and the screen is scaled
  on all cores.
- Settings > This PC: the processor (name, cores and threads; on AMD Ryzen its clock,
  temperature and package power, live), the motherboard, BIOS and each memory module (from the
  firmware's SMBIOS tables), and the board's sensors on Nuvoton NCT6798D/NCT6799D chips (fans,
  temperatures, voltages). See [hardware.md](hardware.md).

## Settings

In sections with a sidebar, saved in Solon storage:

- **Appearance**: background, and text and icon size (Normal / Large / Extra large, the whole
  desktop scaled, which also helps on 4K screens).
- **Date & time**: 12/24-hour clock, the date next to the clock, and setting the date on a
  calendar and the time from drop-down lists.
- **Display**: resolution, the monitor's name and refresh rate, the screens (see above), and
  the screensaver.
- **Sound**: where sound plays, with a Test button for each, and how loud.
- **Mouse** (speed), **Spelling** (language, and the words you added, with Remove), **This
  PC** and **About**.

## Installing and updating

- Boots from a USB stick on UEFI PCs (Secure Boot can stay on, see
  [secureboot/README.md](../secureboot/README.md)) and older BIOS PCs.
- **Installing**: while Solon runs from its USB stick, Disks offers "Install Solon here" on the
  internal disk (with a one-time notice at startup). It erases that disk after two warnings,
  copies Solon's boot area from the stick (so it starts on UEFI and old BIOS PCs) and copies
  everything in the stick's Solon storage. Remove the stick and restart.
- **Updating**: an installed Solon updates itself from a USB drive (Disks, "Update from USB"):
  a newly written Solon stick, or the `solon-usb` files copied onto any USB drive. Only the
  system is replaced; photos, documents and settings stay. Solon says whether the USB copy is
  newer, and a power cut during the update leaves the old or the new version.

## Diagnostics

- F12 shows the log; S saves it (and what the NVIDIA driver read) to the Solon stick.
- A crash screen with what went wrong and the build, for reporting problems.

## Planned

Faster decoding of 1080p and 4K video (all cores); HDMI monitors and their sound; more native
graphics drivers. No network, ever: SimpleOS stays offline on purpose.
