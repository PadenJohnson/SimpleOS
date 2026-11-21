# Assemble the UEFI boot folder (.\esp) with the kernel and photos.
# Photos are loaded into memory by the bootloader (Limine modules) until Solon has USB + FAT32 support.
param(
    [ValidateSet("debug", "release")][string]$Profile = "debug",
    [string[]]$Photos = @(),  # folders of photos to include; defaults to .\samples
    [string]$Cmdline = "",    # kernel command line, e.g. "testprinter=pcl"
    [string]$Resolution = ""  # screen mode to ask the bootloader for, e.g. "1920x1080"
)
$ErrorActionPreference = "Stop"
$root = Split-Path $PSScriptRoot
Set-Location $root

if (-not (Test-Path "limine\BOOTX64.EFI")) {
    throw "Limine binaries not found. Run: git clone https://github.com/limine-bootloader/limine.git --branch=v9.x-binary --depth=1"
}
if (-not $Photos.Count) { $Photos = @("samples") }

if (Test-Path esp) { Remove-Item -Recurse -Force esp }
New-Item -ItemType Directory -Force esp\EFI\BOOT, esp\boot, esp\photos | Out-Null
Copy-Item limine\BOOTX64.EFI esp\EFI\BOOT\BOOTX64.EFI
Copy-Item "target\x86_64-unknown-none\$Profile\solon" esp\boot\kernel

$conf = Get-Content limine.conf -Raw
if ($Cmdline) { $conf = $conf.TrimEnd() + "`n    cmdline: $Cmdline`n" }
if ($Resolution) { $conf = $conf.TrimEnd() + "`n    resolution: $Resolution`n" }
$count = 0
foreach ($dir in $Photos) {
    if (-not (Test-Path $dir)) { Write-Host "Photo folder not found: $dir"; continue }
    foreach ($f in Get-ChildItem $dir -File | Where-Object { $_.Extension -match '^\.(jpe?g|png|avi|wav)$' } | Sort-Object Name) {
        $name = $f.Name -replace '[^A-Za-z0-9._-]', '_'   # Limine paths: keep names simple
        Copy-Item $f.FullName "esp\photos\$name"
        $conf += "    module_path: boot():/photos/$name`n"
        $count++
    }
}
[IO.File]::WriteAllText("$root\esp\boot\limine.conf", $conf)
Write-Host "esp ready: kernel + $count photo(s)"
