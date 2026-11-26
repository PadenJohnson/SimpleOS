# src: the Solon kernel

All of SimpleOS is one Rust program running on the bare machine (`no_std`, Solon's own target
`x86_64-solon.json`: bare-metal x86-64 with the floating-point and vector unit in use). [Limine](https://github.com/limine-bootloader/limine) loads it;
`main.rs` sets the PC up and then runs the desktop's main loop, which polls the drivers between
frames. There are no processes, no system calls and no network. Every file starts with a short
summary of what it does.

## Start-up and the machine

| File | What it is |
|---|---|
| `main.rs` | The entry point: memory, interrupts, screens, drivers and settings, then the desktop's main loop |
| `bootscreen.rs` | The screen while Solon starts: a sunrise, the progress bar (kept moving by another core) and what's happening |
| `memory.rs` | The kernel heap (all of the largest memory region, with quick lists for small blocks), DMA buffers, and mapping device registers |
| `interrupts.rs` | GDT, IDT, the interrupt controller and the 100 Hz timer |
| `smp.rs` | The other processor cores: shared jobs (decoding, drawing) and background tasks (a video's pictures) |
| `cpu.rs`, `smbios.rs`, `hwmon.rs` | The processor, the motherboard's description, and its sensor chip (Settings > This PC) |
| `pci.rs`, `power.rs`, `rtc.rs` | The PCI bus, ACPI power off and restart, the real-time clock |
| `aml.rs`, `battery.rs` | A laptop's battery and charger: the firmware's ACPI code run on a worker core (the embedded controller, a guard against panics), and what the batteries say |
| `serial.rs`, `crash.rs` | The log (serial port and on screen with F12) and the crash screen |

## Screens and drawing

| File | What it is |
|---|---|
| `screen.rs`, `display.rs` | Putting the desktop on each screen (only what changed), resolutions and monitors |
| `gpu.rs`, `nvidia/` | Graphics cards; the NVIDIA driver that lights a second monitor and sends sound to monitors |
| `gfx.rs`, `font.rs`, `raster.rs` | The drawing surface, Noto text and anti-aliased outlines |
| `icons.rs`, `cursor.rs`, `wallpaper.rs`, `scrollbar.rs` | App icons, the pointer (the arrow, and the double arrows for resizing windows), desktop pictures, scroll bars |

## Input, USB and disks

| File | What it is |
|---|---|
| `ps2.rs`, `keys.rs`, `textfield.rs` | Keyboard and mouse, keys for the editors, one-line text boxes |
| `hc.rs`, `xhci.rs`, `ehci.rs`, `usb.rs` | USB 3 and USB 2 controllers, devices, hubs, keyboards and mice |
| `msc.rs`, `phone.rs`, `usbprint.rs`, `usbaudio.rs` | USB drives, phones and cameras (MTP/PTP), printers, sound devices |
| `ahci.rs`, `ide.rs`, `nvme.rs`, `disk.rs`, `fs/` | SATA (AHCI and RAID mode, or IDE mode) and NVMe disks, partition tables, FAT and exFAT |
| `library.rs`, `install.rs`, `update.rs`, `backup.rs` | Solon storage, installing, updating from USB, backups |
| `hda.rs` | Sound: HD Audio controllers, the computer's jacks and the monitors |

## The desktop and its apps

| File | What it is |
|---|---|
| `desktop.rs` | Windows (moved, resized by any edge or corner, maximized), the taskbar on every screen, menus, the clock and wallpaper |
| `settings.rs`, `settingsui.rs` | Saved settings and the Settings window |
| `photos.rs`, `editor.rs`, `edit.rs`, `frame.rs`, `import.rs` | Photos, the photo editor, Photo Frame and the screensaver, Import |
| `files.rs`, `diskmgr.rs` | Files and Disks |
| `videos.rs`, `movie.rs`, `movieui.rs` | Videos and the Video Editor |
| `music.rs`, `soundedit.rs` | Music (songs, albums, artists and the player) and the Sound Editor |
| `paint.rs`, `calendar.rs`, `calculator.rs` | Paint, Calendar, Calculator |
| `printers.rs`, `printui.rs`, `print.rs`, `printlang.rs` | The Printers app, the Print dialog, page layout and printer languages |
| `games/` | Neon Grill Pinball, Starbase Breach, Solitaire, FreeCell, Mines and Sudoku |

## Documents

| File | What it is |
|---|---|
| `docshell.rs`, `docapps.rs` | What Writer, PDF and the Text Editor share |
| `writer/` | Writer and Word (.docx) files |
| `pdf/`, `pdfui.rs` | Reading, drawing, editing and writing PDFs; the PDF app |
| `textedit.rs` | The Text Editor |
| `documents.rs`, `sheetui.rs`, `doc/` | Spreadsheets: the list, the editor, .xlsx/.csv files and formulas |
| `spell.rs` | Spelling for every app that edits text |

## Photos, videos and sound files

| File | What it is |
|---|---|
| `image.rs`, `imgfmt/` | Decoding every photo format (JPEG, PNG, HEIC, WebP, GIF, TIFF, RAW previews, PSD...) |
| `video/` | H.264 encoding and decoding, AAC, FLAC, MP4 and AVI/WAV, for the Video Editor |
| `media/` | The players' engine: containers (MP4, MKV/WebM, AVI, MPEG, Ogg, FLAC, MP3, WAV, AIFF...), decoders (HEVC, VP8, VP9, MPEG-2/4, MP3, AAC, FLAC, Vorbis, Opus, AC-3, DTS...) and song tags; pictures decoded on a core of their own |
