# Developing SimpleOS

SimpleOS (Solon) is a single Rust crate built for its own bare-metal target,
[`x86_64-solon.json`](../x86_64-solon.json), and booted by
[Limine](https://github.com/limine-bootloader/limine). Everything runs in the kernel: drivers,
the desktop and the apps.

The target is Rust's `x86_64-unknown-none` with the processor's floating-point and vector unit
(SSE2) in use instead of software floating point: decoding sound, pictures and fonts runs many
times faster. Every core switches the unit on first thing (`kmain` and `ap_entry`), and AVX2 as
well where the processor has it (`cpu::enable_simd`); interrupt handlers save the registers
they use. The standard library's `core` is built for it from source (`build-std` in
`.cargo/config.toml`, which is why `rust-toolchain.toml` asks for `rust-src`).

## What you need

- Rust: the nightly toolchain (with its source, for `build-std`) is picked automatically
  from `rust-toolchain.toml`. The first build compiles `core` too and takes a few minutes.
- QEMU with OVMF (UEFI firmware), and python3 for the test scripts.
  - Linux (including a Raspberry Pi): `qemu-system-x86`, `ovmf`, `python3`. On a Pi, QEMU
    emulates the PC in software, so booting takes about 25 seconds.
  - Windows: QEMU for Windows; the PowerShell scripts (`run.ps1`, `test.ps1`) find it.
- The Limine binaries, once per clone:

  ```sh
  git clone https://github.com/limine-bootloader/limine.git --branch=v9.x-binary --depth=1
  ```

## Commands

| Command | What it does |
|---|---|
| `./run.sh` / `.\run.ps1` | Build and run in a QEMU window with the sample photos |
| `./test.sh` / `.\test.ps1` | Build and boot headless; drives the mouse and keyboard, takes screenshots, checks the log |
| `tools/make-usb.sh` / `.\tools\make-usb.ps1` | Build the real-PC USB files and disk image into `dist/` |
| `tools/publish-usb.sh` | Upload `dist/` to the GitHub release `latest` (run by the pre-push hook) |
| `tools/get-steamboat-willie.sh` | Download a public-domain test video and copy it onto a Solon stick |
| `tools/make-demo-music.sh` | Make the seven public-domain demo songs (one in each format Music plays) that `make-usb` puts on the stick; needs curl and FFmpeg |
| `.\tools\make-samples.ps1` | Regenerate the sample photos |

Host tools (each its own crate under `tools/`, built for the computer you work on, e.g.
`cargo build --release --target aarch64-unknown-linux-gnu` on a Pi):

| Tool | What it checks or makes |
|---|---|
| `tools/mkimg` | Builds FAT/exFAT/GPT test disks and the USB image, checks partition tables, decodes print jobs, copies files out of disk images (`exfat-get`), looks up crash addresses (`sym`) |
| `tools/mediatest` | The players' decoders, frame by frame and sample by sample against FFmpeg (`audio`, `video`), song tags (`tags`), decoding speed |
| `tools/videotest` | The H.264 encoder and decoder, AAC, FLAC, MP4 and AVI code against Windows' own codecs |
| `tools/pdftest`, `tools/doctest`, `tools/printtest` | PDF reading and writing, Word/Excel files, printer languages |
| `tools/spell` | Builds the spelling dictionary from SCOWL's word lists |

## Testing in QEMU

`./test.sh --help` lists every option; `test.ps1` takes the same ones, written either way
(`--usb kbd mouse` or `-Usb kbd mouse`). The useful ones:

- `--events ...`: input after boot, e.g. `key:esc move:-560,-300 click shot:after type:Hello`
  (screenshots land in `test-out/`).
- `--wait-for "solon: desktop ready"`: go on as soon as a line appears in the serial log.
- `--usb ...`: keyboard, mouse, read-only or writable USB drives, hot-plugging.
- `--internal nvme:<image>` / `sata:<image>`: internal disks (`mkimg library` makes one with
  Solon storage on it).
- `--boot-stick <image>` / `--legacy-bios`: boot from a USB image like a real PC.
- `--sound`: a sound card whose output is saved to `test-out/sound.wav`.
- `--usb audio`: a USB sound device whose output is saved to `test-out/usb-sound.wav`.
- `--music DIR`: songs for the boot drive's `music` folder (like the demo songs; the
  development boot folder isn't a GPT disk, so to see them in Music boot a USB image with
  `--boot-stick`, or put songs in Solon storage with `mkimg library`).
- `--resolution WxH`, `--smp N`, `--cpu MODEL`.
- `--cmdline "..."`: boot options, below.

Boot options (`--cmdline`):

| Option | What it does |
|---|---|
| `testprinter=ipp` (or `urf`, `pcl`, `pclmono`, `pclxl`, `ps`) | A pretend printer whose jobs are captured for checking |
| `testphone` | A pretend phone with photos, a video and a document |
| `secondtest` | The right part of the screen plays a second monitor (for the extended desktop) |
| `novbe` | Leave QEMU's graphics alone, so resolution changes are scaled as on a real PC |
| `soundtest` | Play a tone at start |
| `onecore` | Use only the first processor core |
| `nonvidia`, `nolight` | Don't start the NVIDIA driver; don't light a second monitor |
| `crashtest`, `straytest`, `msrtest` | Check the crash screen, stray interrupts and missing registers |

## The pre-push hook

Pushing `main` runs `.githooks/pre-push` (turn it on once per clone with
`git config core.hooksPath .githooks`; needs Linux, QEMU and a logged-in `gh`): it rebuilds the
USB image, boot-tests it with UEFI and with an old BIOS, and publishes it as the GitHub release
[`latest`](https://github.com/PadenJohnson/SimpleOS/releases/tag/latest), so the download always
matches the code.

## Trying it on a real PC

See [real-pc-guide.md](real-pc-guide.md). F12 shows the log on screen and S saves it (with what
the NVIDIA driver read) to the stick, for looking at afterwards.
