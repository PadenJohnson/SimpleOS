# Graphics cards in Solon: where things stand and the plan

## Today

Solon uses every PC's graphics through the picture the firmware (UEFI) sets up at start-up:
a framebuffer in the card's memory that Solon draws into. That works on every Intel, AMD and
NVIDIA card, with no driver, but the card's own features aren't used.

What Solon already does to use the card well:

- **Recognises the card** (`src/gpu.rs`): vendor, model family and year from the PCI device
  number, and which card shows the screen. Shown in Settings (Display and About) and in the
  log (`gpu: ...`).
- **Write-combining** for the screen memory, so writes to the card go out in bursts (the boot
  loader maps it so; the log line `display: framebuffer memory is cached "WC"` confirms it).
- **Sends only what changed** (`src/screen.rs`): Solon keeps the last picture and writes only
  the changed part of each changed row to the card. Typing in Writer sends about 6% of what
  copying the whole screen each time would (the log reports it once a minute:
  `display: sent ... pixels (..%)`).
- **Instant resolution changes**: on virtual machines' cards (QEMU, VirtualBox, Bochs) Solon's
  own driver switches modes; on other cards lower resolutions are scaled up smoothly by Solon
  (see `src/display.rs`).

## Why "a driver for every card" is a long road

The drivers Linux uses for these cards (i915/xe for Intel, amdgpu for AMD, nouveau for
NVIDIA) are each hundreds of thousands to millions of lines, written by teams at the vendors
over many years. Some parts can't be written independently at all: every NVIDIA card since
2018 (Turing and newer) needs NVIDIA's signed firmware (GSP) for anything, and AMD's newest
display hardware runs AMD firmware too. None of these cards can be emulated in QEMU, so each
step has to be tested on a real PC with that card.

So Solon grows support a feature and a card family at a time, starting with what helps most.

## The plan

| Step | Intel built-in | AMD Radeon | NVIDIA GeForce |
|---|---|---|---|
| Recognise the card, efficient framebuffer use | Done | Done | Done |
| **Screen modes** (any resolution and refresh rate, instantly, native, no scaling) | Display engine driver for Skylake (2015) to current, from Intel's public manuals | AtomBIOS mode setting (the card's own scripts), GCN to Vega (2012-2019); RDNA later and harder | Volta/Turing/Ampere/Ada display engine without firmware, as nouveau did: **started** (see below) |
| **Multiple monitors** | Same driver: more pipes and ports | Same, per card | Same, older cards |
| **Faster drawing** (the card copies and fills) | Blitter / render engine commands | SDMA engine | Older cards' 2D engine |
| **3D** | Very large (shader compiler, command streams); not planned soon | Same | Same, and firmware-limited |

Each driver step starts read-only: the driver reads the card's current state and logs it (so
a report from your PC shows what Solon sees, without risk), then takes over mode setting,
with the firmware's picture kept as the fallback if anything fails.

## NVIDIA driver (src/nvidia.rs)

For GeForce cards from the RTX 20 series on (Volta/Turing/Ampere/Ada "nvdisplay" engine),
following the open nouveau driver's display code (MIT licence). The display engine works
without NVIDIA's signed firmware, so screen modes and several monitors are possible; 3D isn't.

1. **Stage 1: read only (done).** Chip, video memory, the display engine's heads, outputs and
   windows, what each head shows and through which output, and from the VBIOS tables every
   connector and whether a monitor is plugged in. Logged as `nvidia: ...`; F12 then S saves
   the log, the VBIOS (NVVBIOS.ROM) and a display register snapshot (NVREGS.BIN) to the stick.
   Boot option `nonvidia` turns it off.
2. **Stage 2: talk to monitors.** DisplayPort AUX and HDMI DDC: read each monitor's EDID (its
   name and modes) without changing the picture.
3. **Stage 3: take over the display.** Claim it from the firmware, set up the display
   engine's memory, core and window channels, and set a mode on the first monitor.
4. **Stage 4: more monitors.** Light the other heads and outputs (DisplayPort link training,
   HDMI); the desktop spans them.

Each stage is tried on a real card before it's switched on by default.

## Tested PCs

- **GeForce RTX 3080 Founders Edition 10 GB (GA102, Ampere, 2020)**, 1920x1080: recognised
  correctly, works through the firmware's screen. The NVIDIA driver above is being built for
  it. Games (3D) would need NVIDIA's GSP firmware and a full 3D driver: not planned.
  A second monitor may still work without a driver if the PC's firmware switches it on: the
  log says how many screens it set up (`display: the firmware set up N screen(s)`), and
  Settings > Display shows it.

## What's needed from you

Run Solon on each PC and look at Settings > About (or the `gpu:` lines in the diagnostics
log, F12). The names there tell which family to start with. Then each new driver step is
tried on that PC before it's switched on for everyone.
