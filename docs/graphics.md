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
2. **Stage 2: talk to monitors (done).** DisplayPort AUX: each
   monitor's DisplayPort capabilities (DPCD) and EDID (its name and modes), without changing
   the picture. HDMI (DDC over I2C) comes later.
3. **Stage 3: take over the display.** Set up the display engine's memory, core and
   window channels, and light another head and output, while the firmware's picture keeps
   running.
   - 3a (done): find the firmware's picture in video memory (its context DMA in the display
     engine's object table) and save the memory around BAR1's page tables, to place a second
     picture.
   - 3b (on request: F12, then T): start the core channel with a command buffer in system
     memory, run one command that changes nothing, stop it, and put every register back.
     First try on the RTX 3080: the channel started (state 0xb) but never fetched the
     command (state 0xc, GET 0): the firmware leaves PCI bus mastering off, so the card
     couldn't read system memory; the test now turns it on for the duration. It also checks
     BAR1's mode (0xb80f40) and compares the picture read through PRAMIN with the CPU's view
     through BAR1, to tell whether BAR1 maps video memory directly.
     Result on the RTX 3080: the channel read Solon's command (with bus mastering on), and
     BAR1 maps video memory directly (the picture read through PRAMIN matched the CPU's view).
   - 3c (on request: F12, then L; `src/nvidia/light.rs`, `src/nvidia/bios.rs`): light a
     DisplayPort monitor the firmware left dark, following nouveau step by step:
     1. pick the monitor (a DP output whose monitor answers on AUX and that no head shows), a
        free head and its first window, and a free output resource (SOR), as nouveau does;
     2. set up the display engine (tu102_disp_init) with Solon's instance memory: an object
        table and a context DMA covering video memory, plus the identity input and output
        LUTs nouveau gives every window and head on these chips. The block of video memory is
        chosen at run time, clear of what the firmware's screen uses, tested through BAR1,
        and written through BAR1;
     3. start the core, window and window-immediate channels;
     4. route the SOR to the connector's pads and train the DisplayPort link: the VBIOS's
        scripts (spread spectrum, the per-rate scripts, after-training), then clock recovery
        and equalisation with the monitor over AUX, most lanes and fastest rate first;
     5. send the output, head mode and window ownership to the core channel and UPDATE, then
        answer the engine's three supervisor requests: the pixel clock (VPLL), the VBIOS's
        output scripts, the DisplayPort stream settings;
     6. draw a test picture 1 MiB into Solon's block (through BAR1, write-combined) and point
        the window at it, one command at a time, so a stall names its command; then the
        head's output LUT, in a core update of its own.
     First try on the RTX 3080 (build fcd01da): the link trained (4 lanes at 5.4 Gbit/s), the
     mode was set (2560x1440 at 60 Hz) and the LG woke up with a signal, but the screen stayed
     black: the window channel stalled 0x50 bytes into its commands, still executing a method.
     The object table apparently never reached video memory: reading it back through the
     PRAMIN window at 256 MiB gave 0xbad0acXX instead of what was written, so the engine had
     nothing to look the picture's handle up in. Video memory is now tested and written
     through BAR1 instead, and the log says where PRAMIN does and doesn't work.
     Second try (build 5cd963f): the firmware locks video memory from 256 MiB to about
     10 GiB (the MMU lock range, 0x1fa82c/0x1fa830) until NVIDIA's own driver unlocks it,
     so reads there give 0xbad0acXX through BAR1 and PRAMIN alike. Solon's block now sits at
     48 MiB, below the lock, and every word read back right. The window channel took all
     its commands, but the engine rejected the window's UPDATE: "invalid state", code 0x2e.
     The register dump shows the firmware runs its own screen with composition bypass and
     no input LUT, while Solon had switched blending and the input LUT on. Now the window
     tries the firmware's way first, then NVIDIA's driver's way (nvkms: opaque blending,
     identity input LUT, the colour conversion stages off), then opaque without the LUT,
     clearing each rejection and logging its code; and the window's position goes in an
     update of its own first.
     Third try (build ac00e8e): **it works.** The firmware's way (composition bypass, no
     input LUT) was taken at once, and the LG showed Solon's test picture: 2560x1440 at
     59.95 Hz on head 1 through output 0, DisplayPort 4 lanes at 5.4 Gbit/s, colours and
     edges right. The first L found the LG asleep (no answer on AUX at all); a plugged-in
     monitor that doesn't answer is now asked again for up to 1.5 seconds.
     The firmware's monitor isn't touched. The VBIOS's scripts run in Solon's port of
     nouveau's script interpreter, which logs every register they write. If a step fails,
     Solon stops the channels, switches the link off and puts the registers back.
4. **Stage 4: more monitors.** Light the other heads and outputs (DisplayPort link training,
   HDMI); the desktop spans them.
   - 4a (done, worked on the RTX 3080): the lit monitor showed the desktop mirrored, scaled
     to fit with its shape kept (Solon's `Screen` sends only what changed, smoothly scaled),
     with a pointer of its own.
   - 4b (done, waiting for a try on the RTX 3080): the second monitor is lit when Solon starts
     (not with "Main screen only", nor with boot option "nolight"; F12 then L still works) and
     the desktop extends across both. The desktop is one canvas around both screens; each
     screen shows its part (`Screen::present_part`), the main one has the dock, the clock,
     messages and new windows, and the pointer only goes where a screen is (it crosses where
     they meet). Windows keep to the monitor they're on when the arrangement changes and
     maximize on it; each screen has its own wallpaper. Settings > Display > Screens: Extend
     (the default), Mirror or Main screen only; the main screen; where the second monitor
     stands (left, right, above, below) and how the two line up (tops, middles, bottoms, or
     left edges, middles, right edges). Boot option "secondtest" makes the right part of
     QEMU's screen play a second monitor that lights 3 seconds after start.
   - Next: more connectors (HDMI); turning the second monitor's signal off with "Main screen
     only" instead of showing black.

Each stage is tried on a real card before it's switched on by default.

## Tested PCs

- **GeForce RTX 3080 Founders Edition 10 GB (GA102, Ampere, 2020)**, 1920x1080: recognised
  correctly, works through the firmware's screen. Stage 1 read it correctly: 10 GiB, 4 heads,
  4 outputs, 8 windows; head 0 1920x1080 at 60 Hz on output 2 (DisplayPort); connectors
  DisplayPort 1-3 and HDMI, with monitors detected on DisplayPort 2 (the one shown) and
  DisplayPort 3. Stage 2 read both monitors over AUX: an ASUS VG249 on DisplayPort 2
  (DisplayPort 1.1, 4 lanes at 2.7 Gbit/s, 1920x1080 at 60 Hz; its EDID matched the
  firmware's copy byte for byte) and an LG ULTRAGEAR on DisplayPort 3 (DisplayPort 1.2,
  4 lanes at 5.4 Gbit/s, 2560x1440 at 144 Hz). The firmware's display channels are found shut
  down, with head 0 and window 0 still showing its picture from their armed state. The NVIDIA
  driver is being built on it. Games (3D) would need NVIDIA's GSP
  firmware and a full 3D driver: not planned.
  A second monitor may still work without a driver if the PC's firmware switches it on: the
  log says how many screens it set up (`display: the firmware set up N screen(s)`), and
  Settings > Display shows it.

## What's needed from you

Run Solon on each PC and look at Settings > About (or the `gpu:` lines in the diagnostics
log, F12). The names there tell which family to start with. Then each new driver step is
tried on that PC before it's switched on for everyone.
