# tools: building, disk images and tests

Scripts for building the USB image and helpers for testing Solon's code on an ordinary
computer. The Rust tools are separate crates, built for the computer you work on (for example
`cargo build --release --target aarch64-unknown-linux-gnu` on a Raspberry Pi).

| Tool | What it does |
|---|---|
| `make-usb.sh`, `make-usb.ps1` | Build the real-PC USB files (`dist/solon-usb/`) and disk image (`dist/solon.img`) |
| `publish-usb.sh` | Upload `dist/` to the GitHub release `latest` (run by the pre-push hook) |
| `make-esp.sh`, `make-esp.ps1` | Assemble the boot folder for QEMU (`esp/`) with the kernel, test photos and (optionally) songs |
| `make-samples.ps1` | Generate the synthetic test photos in `samples/` |
| `secureboot.ps1` | Give Limine the ".sbat" section shim needs for Secure Boot, and test it (see `secureboot/`) |
| `get-steamboat-willie.sh` | Download Steamboat Willie (1928) and copy it onto a Solon stick, for testing Videos |
| `make-demo-music.sh` | Make the seven public-domain demo songs the USB image carries for Music (downloaded once from Wikimedia Commons, checked, turned into MP3, M4A, Ogg, Opus and FLAC with tags and a cover) |
| `mkimg/` | Disk images: the USB image, FAT/exFAT/GPT test disks, partition checks, print-job and file extraction, crash-address lookup |
| `mediatest/` | The players' decoders against FFmpeg, frame by frame and sample by sample; song tags; decoding speed |
| `videotest/` | The Video Editor's H.264 encoder and decoder, AAC, FLAC, MP4 and AVI code against Windows' own codecs |
| `pdftest/` | Renders PDFs with Solon's PDF reader to PNG files |
| `doctest/` | The spreadsheet engine: formulas, number formats, dates, .xlsx and .csv round trips |
| `printtest/` | Each printer language, encoded and decoded again, checking every pixel survives |
| `spell/` | Builds the spelling dictionary from SCOWL's word lists |

See [docs/development.md](../docs/development.md) for how they fit together.
