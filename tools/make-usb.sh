#!/usr/bin/env bash
# Build Solon for a real PC (bash version of make-usb.ps1; keep the two in step).
#
#   tools/make-usb.sh
#       -> dist/solon-usb/   files to copy onto a FAT32 USB stick (UEFI PCs)
#       -> dist/solon.img    disk image for Rufus / balenaEtcher (UEFI and old BIOS PCs)
#
# Needs a C compiler (for Limine's installer) besides Rust.
set -euo pipefail
cd "$(dirname "$0")/.."

if [ ! -f limine/BOOTX64.EFI ]; then
  echo "Limine binaries not found. Run: git clone https://github.com/limine-bootloader/limine.git --branch=v9.x-binary --depth=1" >&2
  exit 1
fi

quiet() {   # run a build; show its output only if it fails
  local out
  if ! out=$("$@" 2>&1); then printf '%s\n' "$out" >&2; echo "failed: $*" >&2; exit 1; fi
}

echo "1/4 Building the kernel (release)..."
quiet cargo build --release

echo "2/4 Building the image tool and Limine's installer..."
# tools/ would otherwise inherit the kernel's bare-metal target from .cargo/config.toml.
host=$(cd tools/mkimg && rustc -vV | sed -n 's/^host: //p')
(cd tools/mkimg && quiet cargo build --release --target "$host")
mkimg=tools/mkimg/target/$host/release/mkimg
if [ ! -x limine/limine ] || [ limine/limine.c -nt limine/limine ]; then
  quiet make -C limine limine
fi

echo "3/4 Assembling dist/solon-usb ..."
stage=dist/solon-usb
rm -rf "$stage"
mkdir -p "$stage/EFI/BOOT" "$stage/boot"
# Secure Boot (see secureboot/README.md and tools/secureboot.ps1): the firmware starts shim,
# shim starts Limine (with an .sbat section), and the first time shim's key manager adds
# Limine's fingerprint. SOLON-SECURE-BOOT.efi is the same file at the top of the stick.
cp secureboot/shimx64.efi "$stage/EFI/BOOT/BOOTX64.EFI"
cp secureboot/mmx64.efi "$stage/EFI/BOOT/mmx64.efi"
"$mkimg" add-sbat limine/BOOTX64.EFI "$stage/EFI/BOOT/grubx64.efi" secureboot/sbat.csv
cp "$stage/EFI/BOOT/grubx64.efi" "$stage/SOLON-SECURE-BOOT.efi"
cp limine/limine-bios.sys "$stage/boot/limine-bios.sys"
cp target/x86_64-unknown-none/release/solon "$stage/boot/kernel"
# Same boot entry as development, without the test photos.
cp limine.conf "$stage/boot/limine.conf"
# CRLF line ends, like the Windows build, so Notepad shows it properly.
sed 's/$/\r/' > "$stage/SOLON-README.txt" <<'EOF'
Solon - bootable USB files
==========================
Copy everything in this folder (EFI and boot) onto a USB stick formatted as FAT32,
keeping the folder structure. Then boot the PC from the stick.

Secure Boot can stay on. The first time, a blue screen says "Verification failed":
press Enter, then any key. In "Shim UEFI key management" choose "Enroll hash from disk",
the stick (SOLONBOOT), then SOLON-SECURE-BOOT.efi, then "Yes", then "Reboot".
After that Solon starts normally.

See docs\real-pc-guide.md in the Solon project for the full guide.
EOF

echo "4/4 Building dist/solon.img ..."
"$mkimg" usbimg dist/solon.img "$stage"
# Make the image bootable on legacy BIOS PCs too.
limine/limine bios-install dist/solon.img
"$mkimg" check dist/solon.img
# Which commit this is (publish-usb.sh checks it); "+" when built with uncommitted changes.
dirty=""; git diff --quiet HEAD -- || dirty="+"
echo "$(git rev-parse HEAD)$dirty" > dist/BUILD

size=$(du -m dist/solon.img | cut -f1)
echo
echo "Ready:"
echo "  dist/solon-usb/   copy onto a FAT32 stick (UEFI PCs)"
echo "  dist/solon.img    $size MB, write with Rufus or balenaEtcher (UEFI + BIOS)"
