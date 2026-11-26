# Assemble the UEFI boot folder (.\esp) with the kernel and photos.
# make-esp.sh is the Linux version: keep the two in step.
# Photos are loaded into memory by the bootloader (Limine modules); songs (-Music) are copied into
# a "music" folder, which the Music app reads like the demo songs on a real stick.
param(
    [ValidateSet("debug", "release")][string]$Profile = "debug",
    [string[]]$Photos = @(),  # folders of photos to include; defaults to .\samples
    [string]$Music = "",      # a folder of songs to put in the boot drive's "music" folder
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
Copy-Item "target\x86_64-solon\$Profile\solon" esp\boot\kernel

$conf = Get-Content limine.conf -Raw
if ($Cmdline) { $conf = $conf.TrimEnd() + "`n    cmdline: $Cmdline`n" }
if ($Resolution) { $conf = $conf.TrimEnd() + "`n    resolution: $Resolution`n" }
$count = 0
foreach ($dir in $Photos) {
    if (-not (Test-Path $dir)) { Write-Host "Photo folder not found: $dir"; continue }
    foreach ($f in Get-ChildItem $dir -File | Where-Object { $_.Extension -match '^\.(jpe?g|png|gif|bmp|tiff?|webp|heic|heif|psd|tga|ico|ppm|pgm|pbm|pam|dng|cr2|cr3|nef|arw|orf|rw2|raf|pef|srw|avi|wav|mp4|mov)$' } | Sort-Object Name) {
        $name = $f.Name -replace '[^A-Za-z0-9._-]', '_'   # Limine paths: keep names simple
        Copy-Item $f.FullName "esp\photos\$name"
        $conf += "    module_path: boot():/photos/$name`n"
        $count++
    }
}
[IO.File]::WriteAllText("$root\esp\boot\limine.conf", $conf)
$songs = 0
if ($Music) {
    if (Test-Path $Music) {
        New-Item -ItemType Directory -Force esp\music | Out-Null
        Copy-Item (Join-Path $Music "*") esp\music
        $songs = (Get-ChildItem esp\music -File).Count
    } else { Write-Host "Music folder not found: $Music" }
}
Write-Host "esp ready: kernel + $count photo(s) + $songs file(s) in music"
