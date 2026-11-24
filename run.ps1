# Run Solon in a QEMU window (Windows): build the kernel, assemble a UEFI boot folder with the
# sample photos, and start it.
# Usage:  powershell -ExecutionPolicy Bypass -File .\run.ps1
# Options: -Profile release | -Photos C:\my\pictures | -Qemu "C:\path\qemu-system-x86_64.exe" | -Bios "C:\path\OVMF.fd"
param(
    [ValidateSet("debug", "release")][string]$Profile = "debug",
    [string[]]$Photos = @(),
    [string]$Qemu = "",
    [string]$Bios = ""
)
$ErrorActionPreference = "Stop"
Set-Location $PSScriptRoot

# 1. Build the kernel
$ErrorActionPreference = "Continue"
if ($Profile -eq "release") { cargo build --release } else { cargo build }
$ErrorActionPreference = "Stop"
if ($LASTEXITCODE -ne 0) { throw "cargo build failed" }

# 2. Assemble the boot folder (QEMU presents it to the firmware as a FAT disk)
& "$PSScriptRoot\tools\make-esp.ps1" -Profile $Profile -Photos $Photos

# 3. Find QEMU
if (-not $Qemu) {
    $cmd = Get-Command qemu-system-x86_64 -ErrorAction SilentlyContinue
    if ($cmd) { $Qemu = $cmd.Source }
    elseif (Test-Path "C:\Program Files\qemu\qemu-system-x86_64.exe") { $Qemu = "C:\Program Files\qemu\qemu-system-x86_64.exe" }
    else { throw "QEMU not found. Install it from https://www.qemu.org/download/#windows or pass -Qemu <path>." }
}

# 4. Find UEFI firmware (the QEMU Windows installer ships it in its share folder)
$qemuArgs = @("-machine", "q35", "-m", "1G",
              "-drive", "format=raw,file=fat:rw:esp",
              "-serial", "stdio")
if ($Bios) {
    $qemuArgs += @("-bios", $Bios)
} else {
    $share = Join-Path (Split-Path $Qemu) "share"
    $code = Join-Path $share "edk2-x86_64-code.fd"
    $vars = Join-Path $share "edk2-i386-vars.fd"
    if (-not ((Test-Path $code) -and (Test-Path $vars))) {
        Write-Host "UEFI firmware not found in $share."
        Write-Host "Download an OVMF.fd build and run:  .\run.ps1 -Bios C:\path\to\OVMF.fd"
        exit 1
    }
    Copy-Item $vars ovmf_vars.fd -Force
    $qemuArgs += @("-drive", "if=pflash,format=raw,unit=0,readonly=on,file=$code",
                   "-drive", "if=pflash,format=raw,unit=1,file=ovmf_vars.fd")
}

# Click the QEMU window to capture the mouse; Ctrl+Alt+G releases it.
& $Qemu @qemuArgs
