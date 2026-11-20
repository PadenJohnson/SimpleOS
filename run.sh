#!/usr/bin/env bash
# Build the kernel, assemble a UEFI boot folder, and run it in QEMU.
set -euo pipefail
cd "$(dirname "$0")"

PROFILE=${PROFILE:-debug}
if [ "$PROFILE" = release ]; then cargo build --release; else cargo build; fi

LIMINE_DIR=${LIMINE_DIR:-limine}
if [ ! -f "$LIMINE_DIR/BOOTX64.EFI" ]; then
  echo "Limine binaries not found. Run:"
  echo "  git clone https://github.com/limine-bootloader/limine.git --branch=v9.x-binary --depth=1"
  exit 1
fi

rm -rf esp
mkdir -p esp/EFI/BOOT esp/boot
cp "$LIMINE_DIR/BOOTX64.EFI" esp/EFI/BOOT/BOOTX64.EFI
cp limine.conf esp/boot/limine.conf
cp "target/x86_64-unknown-none/$PROFILE/solon" esp/boot/kernel

OVMF=${OVMF:-}
if [ -z "$OVMF" ]; then
  for c in /usr/share/ovmf/OVMF.fd \
           /usr/share/OVMF/OVMF_CODE.fd \
           /usr/share/edk2/ovmf/OVMF_CODE.fd \
           /usr/share/edk2-ovmf/x64/OVMF_CODE.fd \
           /opt/homebrew/share/qemu/edk2-x86_64-code.fd; do
    if [ -f "$c" ]; then OVMF=$c; break; fi
  done
fi
if [ -z "$OVMF" ]; then
  echo "Could not find OVMF firmware. Install it (Debian/Ubuntu: apt install ovmf)"
  echo "or run:  OVMF=/path/to/OVMF.fd ./run.sh"
  exit 1
fi

# Click the QEMU window to capture the mouse; Ctrl+Alt+G releases it.
exec qemu-system-x86_64 \
  -machine q35 -m 1G \
  -bios "$OVMF" \
  -drive format=raw,file=fat:rw:esp \
  -serial stdio
