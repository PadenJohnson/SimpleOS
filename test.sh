#!/usr/bin/env bash
# Headless boot test (Linux, macOS, Raspberry Pi; bash version of test.ps1): build, boot in QEMU
# with no window, drive the mouse and keyboard, take screenshots, and check the serial log for
# expected lines. Needs qemu-system-x86_64, OVMF and python3.
#
# Usage: ./test.sh [options]
# Options take the same names as test.ps1, written either way: --seconds 12 or -Seconds 12.
# List options take several values in a row (until the next option) and can be repeated:
#   ./test.sh --events key:esc move:-560,-300 click shot:after --expect "solon: desktop ready"
#
#   --seconds N         wait this long after starting QEMU before sending input (default 8;
#                       QEMU emulates x86 in software on ARM, so a Pi may need more)
#   --wait-for TEXT     instead of always waiting --seconds, go on as soon as this line is in the
#                       serial log (--seconds is then the limit), e.g. --wait-for "solon: desktop ready"
#   --expect TEXT...    log lines that must appear (default: "solon: kernel entered" "solon: desktop ready")
#   --qemu PATH         QEMU binary (default: qemu-system-x86_64 on PATH)
#   --events E...       input to send after boot, in order: "key:esc", "key:ctrl+s", "type:Hello",
#                       "move:-560,-300" (relative mouse move), "click", "rclick" (right button),
#                       "dblclick", "down", "up",
#                       "wheel:down", "hold:w,1500", "wait:500" (ms), "shot:name" (test-out/name.png),
#                       "plug:<id>", "unplug:<id>"
#   --photos DIR...     photo folders to put on the boot drive (default: ./samples)
#   --usb U...          USB devices: "kbd", "mouse", "disk:<image>" (read-only check: a copy is used
#                       and compared afterwards), "rwdisk:<image>" (used in place, writable),
#                       "hot:<id>=<image>" (ready to plug in later with "plug:<id>")
#   --internal D...     internal disks, used in place: "nvme:<image>", "sata:<image>"
#   --no-boot-photos    boot drive without sample photos
#   --boot-stick IMG    boot like a real PC from this image as the only USB stick (writable, in place)
#   --legacy-bios       boot with SeaBIOS instead of UEFI
#   --cmdline TEXT      kernel command line (development boot only), e.g. "testprinter=pcl"
#   --resolution WxH    screen mode (development boot only), e.g. "1920x1080"
#   --smp N             N processor cores (threads) instead of one
#   --cpu MODEL         QEMU processor model, e.g. "max" (has MONITOR/MWAIT, which the other cores
#                       sleep in)
#   --stick-on-usb2     with --boot-stick: put the stick on a USB 2 controller Solon can't use
#   --no-esp            no development boot folder: boot from an internal disk (after installing)
#   --sound             add an Intel HD Audio card; what it plays lands in test-out/sound.wav
#   --expect-exit       the test ends with Solon turning the machine off or restarting
#   --secure-boot       Secure Boot on with Microsoft's keys. Uses test-out/sb/OVMF_CODE.secboot.fd and
#                       OVMF_VARS.secboot.fd if present, otherwise Debian's OVMF_CODE_4M.secboot.fd and
#                       OVMF_VARS_4M.ms.fd. Enrolled keys are kept in test-out/sb/vars.fd between runs.
#   --reset-secure-boot start the Secure Boot firmware settings over
#
# Environment: OVMF_CODE / OVMF_VARS override the UEFI firmware files.
set -uo pipefail
cd "$(dirname "$0")"
root=$(pwd)

seconds=8 wait_for="" qemu=qemu-system-x86_64 boot_stick="" cmdline="" resolution="" smp="" cpu=""
no_boot_photos=0 legacy_bios=0 stick_on_usb2=0 no_esp=0 sound=0 expect_exit=0 secure_boot=0 reset_sb=0
expect=() events=() photos=() usb=() internal=() expect_set=0

die() { echo "test.sh: $*" >&2; exit 2; }
# The values of a list option: every following argument that isn't an option.
take() { list=(); while [ $# -gt 0 ] && [[ $1 != -[A-Za-z-]* ]]; do list+=("$1"); shift; done; }
while [ $# -gt 0 ]; do
  # --no-boot-photos, -NoBootPhotos and --nobootphotos all become "nobootphotos"
  opt=$(printf '%s' "$1" | sed 's/^--\{0,1\}//; s/-//g' | tr 'A-Z' 'a-z'); shift
  case "$opt" in
    seconds) seconds=${1:?}; shift ;;
    qemu) qemu=${1:?}; shift ;;
    waitfor) wait_for=${1:?}; shift ;;
    bootstick) boot_stick=${1:?}; shift ;;
    cmdline) cmdline=${1?}; shift ;;
    resolution) resolution=${1?}; shift ;;
    smp) smp=${1:?}; shift ;;
    cpu) cpu=${1:?}; shift ;;
    expect|events|event|photos|usb|internal)
      take "$@"; shift ${#list[@]}
      [ ${#list[@]} -gt 0 ] || die "--$opt needs a value"
      case "$opt" in
        expect) [ $expect_set = 0 ] && expect=(); expect_set=1; expect+=("${list[@]}") ;;
        events|event) events+=("${list[@]}") ;;
        photos) photos+=("${list[@]}") ;;
        usb) usb+=("${list[@]}") ;;
        internal) internal+=("${list[@]}") ;;
      esac ;;
    nobootphotos) no_boot_photos=1 ;;
    legacybios) legacy_bios=1 ;;
    stickonusb2) stick_on_usb2=1 ;;
    noesp) no_esp=1 ;;
    sound) sound=1 ;;
    expectexit) expect_exit=1 ;;
    secureboot) secure_boot=1 ;;
    resetsecureboot) reset_sb=1 ;;
    h|help) sed -n '2,/^set -uo/p' "$0" | sed '$d; s/^# \{0,1\}//'; exit 0 ;;
    *) die "unknown option -$opt (see ./test.sh --help)" ;;
  esac
done
[ $expect_set = 1 ] || expect=("solon: kernel entered" "solon: desktop ready")
command -v "$qemu" >/dev/null || die "QEMU not found ($qemu). Debian: sudo apt install qemu-system-x86"
command -v python3 >/dev/null || die "python3 not found"

# QEMU option values: a comma in a file name must be doubled.
q() { printf '%s' "${1//,/,,}"; }

# 1. Build
out=$(cargo build 2>&1); code=$?
if [ $code -ne 0 ]; then printf '%s\n' "$out"; echo "cargo build failed" >&2; exit 1; fi
printf '%s\n' "$out" | grep -E '^(warning|error)' || true

# 2. Boot folder
if [ -z "$boot_stick" ]; then
  esp_args=(--cmdline "$cmdline" --resolution "$resolution")
  if [ $no_boot_photos = 1 ]; then esp_args+=(--photos nonexistent-folder)
  else for p in "${photos[@]}"; do esp_args+=(--photos "$p"); done; fi
  tools/make-esp.sh "${esp_args[@]}" || exit 1
fi

mkdir -p test-out
out_dir=$root/test-out
log=test-out/serial.log
# COM2 output (the hidden test printer's print jobs) lands in test-out/printjob.bin.
rm -f "$log" test-out/*.png test-out/*.ppm test-out/printjob.bin test-out/qmp.sock

# 3. Firmware
find_fw() { for c in "$@"; do [ -f "$c" ] && { echo "$c"; return; }; done; }
qemu_share=$(dirname "$(command -v "$qemu")")/../share/qemu
args=(-machine q35 -m 1G -display none
      -serial "file:$log"
      -serial file:test-out/printjob.bin
      -qmp "unix:$out_dir/qmp.sock,server=on,wait=off")
[ -n "$smp" ] && args+=(-smp "$smp")
[ -n "$cpu" ] && args+=(-cpu "$cpu")
# Environment: QEMU_EXTRA adds QEMU options (e.g. "-accel tcg,thread=multi").
[ -n "${QEMU_EXTRA:-}" ] && read -r -a extra <<< "$QEMU_EXTRA" && args+=("${extra[@]}")
if [ $secure_boot = 1 ]; then
  mkdir -p test-out/sb
  if [ -f test-out/sb/OVMF_CODE.secboot.fd ]; then
    sb_code=test-out/sb/OVMF_CODE.secboot.fd sb_vars=test-out/sb/OVMF_VARS.secboot.fd
  else
    sb_code=$(find_fw /usr/share/OVMF/OVMF_CODE_4M.secboot.fd /usr/share/edk2/ovmf/OVMF_CODE.secboot.fd)
    sb_vars=$(find_fw /usr/share/OVMF/OVMF_VARS_4M.ms.fd /usr/share/edk2/ovmf/OVMF_VARS.secboot.fd)
  fi
  [ -n "$sb_code" ] && [ -f "$sb_vars" ] || die "Secure Boot firmware not found: put Fedora's OVMF_CODE.secboot.fd and OVMF_VARS.secboot.fd in test-out/sb/ (or Debian: sudo apt install ovmf)"
  if [ $reset_sb = 1 ] || [ ! -f test-out/sb/vars.fd ]; then cp "$sb_vars" test-out/sb/vars.fd; fi
  args[1]=q35,smm=on
  args+=(-global driver=cfi.pflash01,property=secure,value=on
         -drive "if=pflash,format=raw,unit=0,readonly=on,file=$(q "$sb_code")"
         -drive if=pflash,format=raw,unit=1,file=test-out/sb/vars.fd)
elif [ $legacy_bios = 0 ]; then
  code_fd=${OVMF_CODE:-$(find_fw "$qemu_share/edk2-x86_64-code.fd" /usr/share/OVMF/OVMF_CODE_4M.fd \
                          /usr/share/edk2/ovmf/OVMF_CODE.fd /usr/share/edk2-ovmf/x64/OVMF_CODE.fd \
                          /usr/share/OVMF/OVMF_CODE.fd)}
  case "$code_fd" in
    */edk2-x86_64-code.fd) vars_def=$(dirname "$code_fd")/edk2-i386-vars.fd ;;
    *) vars_def=${code_fd/CODE/VARS} ;;
  esac
  vars_fd=${OVMF_VARS:-$vars_def}
  [ -n "$code_fd" ] && [ -f "$code_fd" ] && [ -f "$vars_fd" ] ||
    die "UEFI firmware not found (Debian: sudo apt install ovmf), or set OVMF_CODE and OVMF_VARS"
  cp "$vars_fd" test-out/vars.fd
  args+=(-drive "if=pflash,format=raw,unit=0,readonly=on,file=$(q "$code_fd")"
         -drive if=pflash,format=raw,unit=1,file=test-out/vars.fd)
fi

# 4. Disks and devices
[ -z "$boot_stick" ] && [ $no_esp = 0 ] && args+=(-drive format=raw,file=fat:rw:esp)
xhci=0
if [ $sound = 1 ]; then
  rm -f test-out/sound.wav
  args+=(-audiodev wav,id=snd0,path=test-out/sound.wav -device intel-hda -device hda-output,audiodev=snd0)
fi
if [ -n "$boot_stick" ] && [ $stick_on_usb2 = 1 ]; then
  # The stick on a USB 2 (EHCI) controller, which Solon doesn't drive (like some laptops' ports).
  args+=(-device usb-ehci,id=ehci
         -drive "if=none,id=bootstick,format=raw,file=$(q "$boot_stick")"
         -device usb-storage,bus=ehci.0,drive=bootstick,bootindex=0)
elif [ -n "$boot_stick" ]; then
  args+=(-device qemu-xhci,id=xhci
         -drive "if=none,id=bootstick,format=raw,file=$(q "$boot_stick")"
         -device usb-storage,bus=xhci.0,drive=bootstick,bootindex=0)
  xhci=1
fi
i=0
for d in "${internal[@]}"; do
  kind=${d%%:*} file=${d#*:}
  args+=(-drive "if=none,id=int$i,format=raw,file=$(q "$file")")
  if [ "$kind" = nvme ]; then args+=(-device "nvme,serial=SOLONTEST$i,drive=int$i")
  else args+=(-device "ide-hd,drive=int$i,bus=ide.$((i + 2))"); fi
  i=$((i + 1))
done
disk_checks=()   # pairs: original, copy
if [ ${#usb[@]} -gt 0 ]; then
  [ $xhci = 1 ] || args+=(-device qemu-xhci,id=xhci)
  n=0
  for u in "${usb[@]}"; do
    kind=${u%%:*} rest=${u#*:}
    case "$kind" in
      kbd) args+=(-device usb-kbd,bus=xhci.0) ;;
      mouse) args+=(-device usb-mouse,bus=xhci.0) ;;
      disk)
        # Writable copy: the firmware (OVMF) crashes on read-only USB disks at boot.
        # Comparing the copy afterwards proves Solon never wrote to the drive.
        copy=test-out/usb-disk$n.img
        cp "$rest" "$copy" || exit 1
        disk_checks+=("$rest" "$copy")
        args+=(-drive "if=none,id=disk$n,format=raw,file=$(q "$copy")"
               -device "usb-storage,bus=xhci.0,drive=disk$n,id=dev-disk$n")
        n=$((n + 1)) ;;
      rwdisk)
        # Used in place and writable (for tests where Solon writes to the drive).
        args+=(-drive "if=none,id=disk$n,format=raw,file=$(q "$rest")"
               -device "usb-storage,bus=xhci.0,drive=disk$n,id=dev-disk$n")
        n=$((n + 1)) ;;
      hot)
        id=${rest%%=*} file=${rest#*=}
        args+=(-drive "if=none,id=$id,format=raw,readonly=on,file=$(q "$file")") ;;
      *) die "unknown USB device: $u" ;;
    esac
  done
fi

# 5. Boot and drive it
"$qemu" "${args[@]}" > test-out/qemu.log 2>&1 &
qpid=$!
trap 'kill -9 $qpid 2>/dev/null' EXIT

python3 - "$out_dir" "$seconds" "$expect_exit" "$qpid" "$wait_for" "${events[@]}" <<'PY'
import json, os, socket, sys, time
out, seconds, expect_exit, qpid = sys.argv[1], float(sys.argv[2]), sys.argv[3] == "1", int(sys.argv[4])
wait_for, events = sys.argv[5].encode(), sys.argv[6:]
ms = lambda n: time.sleep(n / 1000)

def alive():
    try:
        return os.waitpid(qpid, os.WNOHANG) == (0, 0)
    except ChildProcessError:
        try: os.kill(qpid, 0); return True
        except OSError: return False

class Qmp:
    def __init__(self):
        self.s = socket.socket(socket.AF_UNIX)
        for _ in range(100):   # QEMU may still be creating the socket
            try: self.s.connect(os.path.join(out, "qmp.sock")); break
            except OSError: time.sleep(0.1)
        else: raise ConnectionError("could not connect to QEMU's QMP socket")
        self.f = self.s.makefile("rw")
        self.line()   # greeting
        self.cmd("qmp_capabilities")
    def line(self):
        l = self.f.readline()
        if not l: raise ConnectionError("QEMU closed the QMP connection")
        return l
    def cmd(self, name, **arguments):
        msg = {"execute": name}
        if arguments: msg["arguments"] = arguments
        self.f.write(json.dumps(msg) + "\n"); self.f.flush()
        while True:
            r = json.loads(self.line())
            if "event" in r: continue
            if "error" in r: print(f"QMP {name}: {r['error'].get('desc')}", file=sys.stderr)
            return r

qmp = None
def Q(name, **a):
    global qmp
    if qmp is None: qmp = Qmp()
    return qmp.cmd(name, **a)
def keys(names, **extra): return Q("send-key", keys=[{"type": "qcode", "data": k} for k in names], **extra)
# The mouse goes through monitor commands: Linux builds of QEMU (10.0, -display none) accept
# QMP's input-send-event but don't pass it on to the guest.
def hmp(line): Q("human-monitor-command", **{"command-line": line}); ms(60)
held = 0
def button(down, b="left"):
    global held
    bit = {"left": 1, "right": 2, "middle": 4}[b]
    held = held | bit if down else held & ~bit
    hmp(f"mouse_button {held}")
def shot(name): Q("screendump", filename=os.path.join(out, name + ".ppm")); ms(300)

SHIFTED = {'!': '1', '@': '2', '#': '3', '$': '4', '%': '5', '(': '9', ')': '0', '?': 'slash',
           ':': 'semicolon', '"': 'apostrophe', '_': 'minus', '+': 'equal'}
PLAIN = {' ': 'spc', '=': 'equal', '-': 'minus', '.': 'dot', ',': 'comma', ';': 'semicolon',
         '/': 'slash', "'": 'apostrophe'}

try:
    if wait_for:
        end, start = time.time() + seconds, time.time()
        log = os.path.join(out, "serial.log")
        while time.time() < end and alive():
            try:
                if wait_for in open(log, "rb").read(): break
            except FileNotFoundError: pass
            time.sleep(0.25)
        else:
            print(f"test.sh: no {wait_for.decode()!r} in the log after {seconds:g} s", file=sys.stderr)
        print(f"waited {time.time() - start:.1f} s for the log")
        time.sleep(0.5)
    else:
        time.sleep(seconds)
    for e in events:
        kind, _, arg = e.partition(":")
        if kind == "key":
            keys(arg.split("+")); ms(120)          # "key:a", "key:shift+9", "key:ctrl+s"
        elif kind == "type":
            for ch in arg:
                if ch in SHIFTED: k = ["shift", SHIFTED[ch]]
                elif ch.isupper(): k = ["shift", ch.lower()]
                else: k = [PLAIN.get(ch, ch)]
                keys(k); ms(60)
        elif kind == "move":
            # PS/2 packets carry at most ~127 per axis, so send big moves in small steps.
            dx, dy = (int(v) for v in arg.split(","))
            steps = max(1, -(-max(abs(dx), abs(dy)) // 40))
            sx = sy = 0
            for i in range(1, steps + 1):
                nx = round(dx * i / steps) - sx; ny = round(dy * i / steps) - sy
                sx += nx; sy += ny
                hmp(f"mouse_move {nx} {ny}")
            ms(200)
        elif kind == "click": button(True); button(False); ms(200)
        elif kind == "rclick": button(True, "right"); button(False, "right"); ms(300)
        elif kind == "dblclick": button(True); button(False); button(True); button(False); ms(300)
        elif kind == "wheel":                       # wheel:down or wheel:up (one notch)
            hmp(f"mouse_move 0 0 {1 if arg == 'up' else -1}"); ms(100)
        elif kind == "plug": Q("device_add", driver="usb-storage", bus="xhci.0", drive=arg, id="dev-" + arg)
        elif kind == "unplug": Q("device_del", id="dev-" + arg)
        elif kind == "down": button(True)
        elif kind == "up": button(False)
        elif kind == "wait": ms(int(arg))
        elif kind == "hold":                        # hold:w,1500 holds a key down for 1.5 s
            k, t = arg.split(",")
            keys([k], **{"hold-time": int(t)}); ms(int(t) + 100)
        elif kind == "shot": shot(arg)
        else: print(f"unknown event: {e}", file=sys.stderr)
    ms(500)
    if expect_exit:
        # Solon should have switched the machine off (QEMU exits) or restarted it.
        time.sleep(3)
        print("QEMU " + ("still running" if alive() else "exited (machine turned off)"))
    else:
        shot("screen")
except (OSError, ConnectionError, ValueError) as ex:
    if not expect_exit:
        print(f"test.sh: lost QEMU: {ex}", file=sys.stderr); sys.exit(1)
    print("QEMU went away during the test (expected: machine turned off)")
PY
drive_rc=$?
kill -9 $qpid 2>/dev/null; wait $qpid 2>/dev/null
trap - EXIT
if [ -s test-out/qemu.log ]; then echo "---- qemu output ----"; cat test-out/qemu.log; fi

# 6. Convert PPM screenshots -> PNG so they can be viewed easily.
python3 - test-out/*.ppm <<'PY'
import os, struct, sys, zlib
for path in sys.argv[1:]:
    if not os.path.exists(path): continue
    b = open(path, "rb").read()
    fields, i = [], 0
    while len(fields) < 4:
        while b[i:i+1].isspace(): i += 1
        j = i
        while not b[j:j+1].isspace(): j += 1
        fields.append(b[i:j]); i = j
    i += 1
    w, h = int(fields[1]), int(fields[2])
    px = b[i:i + w * h * 3]
    raw = b"".join(b"\0" + px[y*w*3:(y+1)*w*3] for y in range(h))
    chunk = lambda t, d: struct.pack(">I", len(d)) + t + d + struct.pack(">I", zlib.crc32(t + d))
    png = b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", struct.pack(">IIBBBBB", w, h, 8, 2, 0, 0, 0)) \
        + chunk(b"IDAT", zlib.compress(raw, 6)) + chunk(b"IEND", b"")
    open(path[:-4] + ".png", "wb").write(png)
    os.remove(path)
PY

# 7. Check the log
text=""; [ -f "$log" ] && text=$(cat "$log")
echo "---- serial log ----"; printf '%s\n' "$text"
fail=()
for e in "${expect[@]}"; do [[ $text == *"$e"* ]] || fail+=("$e"); done
[[ $text == *"KERNEL PANIC"* ]] && fail+=("no kernel panic")
[[ $text == *"X64 Exception"* ]] && fail+=("no firmware crash")
[ $drive_rc -ne 0 ] && fail+=("QEMU running until the end of the test")
for ((k = 0; k < ${#disk_checks[@]}; k += 2)); do
  orig=${disk_checks[k]} copy=${disk_checks[k+1]}
  if cmp -s "$orig" "$copy"; then echo "USB disk $orig: unchanged (read-only OK)"
  else fail+=("USB disk $orig left unmodified"); fi
done
if [ ${#fail[@]} -gt 0 ]; then
  msg=$(printf '%s, ' "${fail[@]}")
  printf '\e[31mTEST FAILED, missing: %s\e[0m\n' "${msg%, }"
  exit 1
fi
printf '\e[32mTEST PASSED\e[0m\n'
