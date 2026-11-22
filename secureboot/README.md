# Secure Boot

Solon starts on PCs with Secure Boot turned on like this:

1. The PC's firmware starts **shim** (`EFI/BOOT/BOOTX64.EFI`). Shim is signed by Microsoft,
   so every PC that trusts Microsoft's third-party UEFI key starts it.
2. Shim starts **Limine**, Solon's boot loader (`EFI/BOOT/grubx64.efi`, the name shim looks
   for). Limine carries an `.sbat` section (`sbat.csv`, added by `mkimg add-sbat`), which shim
   requires.
3. The first time, shim doesn't know Limine, so it opens its key manager (MokManager,
   `EFI/BOOT/mmx64.efi`): choose **Enroll hash from disk**, pick `SOLON-SECURE-BOOT.efi` (the
   same file as `grubx64.efi`, at the top of the stick), Continue, Yes, Reboot. From then on
   Solon just starts. The fingerprint only changes when Limine is updated.

Why a fingerprint and not a signing key: when shim approves the next program by a key, it
requires that program to have the kernel checked by shim too, and halts with "Bootloader
has not verified loaded image" otherwise. Limine doesn't do that; programs approved by
fingerprint are exempt. (Tested with Fedora's OVMF Secure Boot firmware with Microsoft's
keys: `test.ps1 -SecureBoot -BootStick <copy of dist\solon.img>`; on Linux
`./test.sh --secure-boot --boot-stick <copy of dist/solon.img>`, which falls back to Debian's
`ovmf` Secure Boot firmware with Microsoft's keys.)

## Files

| File | What it is |
|---|---|
| `shimx64.efi` | shim 15.8, signed by Microsoft, from Fedora 43's `shim-x64-15.8-3.x86_64.rpm` (`/boot/efi/EFI/fedora/shimx64.efi`) |
| `mmx64.efi` | MokManager from the same package |
| `sbat.csv` | The SBAT data added to Limine |

Downloaded 2026-11-21 from https://mirrors.mit.edu/fedora/linux/releases/43/Everything/x86_64/os/Packages/s/shim-x64-15.8-3.x86_64.rpm
(rpm SHA-256 `b6602bf8420e56de84653c476bf25f6e26dd28a9ed3487d38cf0900ae008750a`).

SHA-256 of the files here:

    4773d74d87c2371a25883b59a3b6d98d157de46933676706d215015b1130f2d1  shimx64.efi
    83a49149f5a7060d0de43de3a0c683d99f3c58c418ab651ac9c254b4efaef0b8  mmx64.efi

shim is open source under a BSD-style licence: https://github.com/rhboot/shim
(source: Fedora's `shim` package, https://src.fedoraproject.org/rpms/shim).
