# Build Solon for a real PC.
#
#   powershell -ExecutionPolicy Bypass -File .\tools\make-usb.ps1
#       -> dist\solon-usb\   files to copy onto a FAT32 USB stick (UEFI PCs)
#       -> dist\solon.img    disk image for Rufus / balenaEtcher (UEFI and old BIOS PCs)
#
#   powershell -ExecutionPolicy Bypass -File .\tools\make-usb.ps1 -CopyTo E:
#       also copies the files onto the FAT32 USB stick at E: (nothing is formatted or deleted)
param(
    [string]$CopyTo = ""
)
$ErrorActionPreference = "Stop"
$root = Split-Path $PSScriptRoot
Set-Location $root

Write-Host "1/4 Building the kernel (release)..."
$ErrorActionPreference = "Continue"
$out = cmd /c "cargo build --release 2>&1"
$code = $LASTEXITCODE
$ErrorActionPreference = "Stop"
if ($code -ne 0) { $out | Write-Host; throw "cargo build failed" }

Write-Host "2/4 Building the image tool..."
Push-Location tools\mkimg
$ErrorActionPreference = "Continue"
$out = cmd /c "cargo build --release 2>&1"
$code = $LASTEXITCODE
$ErrorActionPreference = "Stop"
Pop-Location
if ($code -ne 0) { $out | Write-Host; throw "mkimg build failed" }
$mkimg = "tools\mkimg\target\x86_64-pc-windows-msvc\release\mkimg.exe"

Write-Host "3/4 Assembling dist\solon-usb ..."
$stage = "dist\solon-usb"
if (Test-Path $stage) { Remove-Item -Recurse -Force $stage }
New-Item -ItemType Directory -Force "$stage\EFI\BOOT", "$stage\boot" | Out-Null
# Secure Boot (see secureboot\README.md): the firmware starts shim, shim starts Limine
# (with an .sbat section), and the first time shim's key manager adds Limine's fingerprint.
# SOLON-SECURE-BOOT.efi is the same file at the top of the stick, easy to pick.
. "$PSScriptRoot\secureboot.ps1"
Copy-Item secureboot\shimx64.efi "$stage\EFI\BOOT\BOOTX64.EFI"
Copy-Item secureboot\mmx64.efi "$stage\EFI\BOOT\mmx64.efi"
Invoke-SolonSecureLoader "limine\BOOTX64.EFI" "$stage\EFI\BOOT\grubx64.efi" $mkimg
Copy-Item "$stage\EFI\BOOT\grubx64.efi" "$stage\SOLON-SECURE-BOOT.efi"
Copy-Item limine\limine-bios.sys "$stage\boot\limine-bios.sys"
Copy-Item target\x86_64-unknown-none\release\solon "$stage\boot\kernel"
# Same boot entry as development, without the test photos.
Copy-Item limine.conf "$stage\boot\limine.conf"
@"
Solon - bootable USB files
==========================
Copy everything in this folder (EFI and boot) onto a USB stick formatted as FAT32,
keeping the folder structure. Then boot the PC from the stick.

Secure Boot can stay on. The first time, a blue screen says "Verification failed":
press Enter, then any key. In "Shim UEFI key management" choose "Enroll hash from disk",
the stick (SOLONBOOT), then SOLON-SECURE-BOOT.efi, then "Yes", then "Reboot".
After that Solon starts normally.

See docs\real-pc-guide.md in the Solon project for the full guide.
"@ | Out-File -Encoding ascii "$stage\SOLON-README.txt"

Write-Host "4/4 Building dist\solon.img ..."
& $mkimg usbimg dist\solon.img $stage
if ($LASTEXITCODE -ne 0) { throw "image build failed" }
# Make the image bootable on legacy BIOS PCs too.
$ErrorActionPreference = "Continue"
$bios = & limine\limine.exe bios-install dist\solon.img 2>&1
$code = $LASTEXITCODE
$ErrorActionPreference = "Stop"
$bios | Write-Host
if ($code -ne 0) { throw "limine bios-install failed" }
& $mkimg check dist\solon.img
if ($LASTEXITCODE -ne 0) { throw "image check failed" }

if ($CopyTo) {
    $letter = $CopyTo.TrimEnd(':', '\') + ":"
    $disk = Get-CimInstance Win32_LogicalDisk -Filter "DeviceID='$letter'"
    if (-not $disk) { throw "No drive $letter found." }
    if ($disk.DriveType -ne 2) { throw "$letter is not a removable drive; refusing to copy." }
    if ($disk.FileSystem -ne "FAT32") { throw "$letter is formatted as $($disk.FileSystem). Format it as FAT32 first (this script never formats)." }
    Write-Host "Copying Solon onto $letter ..."
    Copy-Item -Recurse -Force "$stage\*" "$letter\"
    Write-Host "Done. Safely eject $letter, then boot the PC from it."
}

$img = Get-Item dist\solon.img
Write-Host ""
Write-Host "Ready:"
Write-Host "  dist\solon-usb\   copy onto a FAT32 stick (UEFI PCs)"
Write-Host ("  dist\solon.img    {0:N1} MB, write with Rufus or balenaEtcher (UEFI + BIOS)" -f ($img.Length / 1MB))
