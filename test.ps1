# Headless boot test: build, boot in QEMU with no window, capture serial log + a screenshot, check for expected log lines.
# Usage:  powershell -ExecutionPolicy Bypass -File .\test.ps1 [-Seconds 8] [-Expect "solon: desktop ready"]
param(
    [int]$Seconds = 8,
    [string[]]$Expect = @("solon: kernel entered", "solon: desktop ready"),
    [string]$Qemu = "C:\Program Files\qemu\qemu-system-x86_64.exe",
    # Input to send after boot, in order: "key:esc", "move:-560,-300" (relative mouse move),
    # "click", "dblclick", "wait:500" (ms), "shot:name" (extra screenshot test-out\name.png)
    [string[]]$Events = @(),
    [string[]]$Photos = @(),  # photo folders to put on the boot drive (default: .\samples)
    # USB devices: "kbd", "mouse", "disk:<image file>" (plugged in at boot),
    # "hot:<id>=<image file>" (ready to plug in later with the "plug:<id>" event; "unplug:<id>" removes it)
    [string[]]$Usb = @(),
    # Internal disks, used in place (writable, so they persist between runs):
    # "nvme:<image file>", "sata:<image file>"
    [string[]]$Internal = @(),
    [switch]$NoBootPhotos,    # boot drive without sample photos
    # Boot like a real PC: from this disk image attached as the only USB stick (used in
    # place, writable), instead of the development boot folder.
    [string]$BootStick = "",
    [switch]$LegacyBios,      # boot with SeaBIOS (old BIOS PCs) instead of UEFI
    [string]$Cmdline = ""     # kernel command line (development boot only), e.g. "testprinter=pcl"
)
$ErrorActionPreference = "Stop"
Set-Location $PSScriptRoot

$ErrorActionPreference = "Continue"
$out = cmd /c "cargo build 2>&1"
$code = $LASTEXITCODE
$ErrorActionPreference = "Stop"
if ($code -ne 0) { $out | Write-Host; throw "cargo build failed" }
$out | Where-Object { $_ -match "^(warning|error)" } | Write-Host

if ($NoBootPhotos) { $Photos = @("nonexistent-folder") }
if (-not $BootStick) { & "$PSScriptRoot\tools\make-esp.ps1" -Photos $Photos -Cmdline $Cmdline }

New-Item -ItemType Directory -Force test-out | Out-Null
$log = "test-out\serial.log"
# COM2 output (the hidden test printer's print jobs) lands in test-out\printjob.bin.
Remove-Item $log, test-out\*.png, test-out\*.ppm, test-out\printjob.bin -ErrorAction SilentlyContinue

$share = Join-Path (Split-Path $Qemu) "share"
Copy-Item (Join-Path $share "edk2-i386-vars.fd") test-out\vars.fd -Force
$port = 4444
$args = @("-machine", "q35", "-m", "1G", "-display", "none",
          "-serial", "file:$log",
          "-serial", "file:test-out\printjob.bin",
          "-qmp", "tcp:127.0.0.1:$port,server,nowait")
if (-not $LegacyBios) {
    $args += @("-drive", "`"if=pflash,format=raw,unit=0,readonly=on,file=$(Join-Path $share 'edk2-x86_64-code.fd')`"",
               "-drive", "if=pflash,format=raw,unit=1,file=test-out\vars.fd")
}
if (-not $BootStick) { $args += @("-drive", "format=raw,file=fat:rw:esp") }
$xhci = $false
if ($BootStick) {
    $args += @("-device", "qemu-xhci,id=xhci",
               "-drive", "`"if=none,id=bootstick,format=raw,file=$BootStick`"",
               "-device", "usb-storage,bus=xhci.0,drive=bootstick,bootindex=0")
    $xhci = $true
}
$i = 0
foreach ($d in $Internal) {
    $kind, $file = $d -split ':', 2
    $args += @("-drive", "`"if=none,id=int$i,format=raw,file=$file`"")
    if ($kind -eq "nvme") { $args += @("-device", "nvme,serial=SOLONTEST$i,drive=int$i") }
    else { $args += @("-device", "ide-hd,drive=int$i,bus=ide.$($i + 2)") }
    $i++
}
$script:diskChecks = @()
if ($Usb.Count) {
    if (-not $xhci) { $args += @("-device", "qemu-xhci,id=xhci") }
    $n = 0
    foreach ($u in $Usb) {
        $kind, $rest = $u -split ':', 2
        switch ($kind) {
            "kbd"   { $args += @("-device", "usb-kbd,bus=xhci.0") }
            "mouse" { $args += @("-device", "usb-mouse,bus=xhci.0") }
            "disk"  {
                # Writable copy: the firmware (OVMF) crashes on read-only USB disks at boot.
                # Comparing the copy afterwards proves Solon never wrote to the drive.
                $copy = "test-out\usb-disk$n.img"
                Copy-Item $rest $copy -Force
                $script:diskChecks += ,@($rest, $copy)
                $args += @("-drive", "`"if=none,id=disk$n,format=raw,file=$copy`"",
                           "-device", "usb-storage,bus=xhci.0,drive=disk$n,id=dev-disk$n")
                $n++
            }
            "hot"   {
                $id, $file = $rest -split '=', 2
                $args += @("-drive", "`"if=none,id=$id,format=raw,readonly=on,file=$file`"")
            }
        }
    }
}
$p = Start-Process -FilePath $Qemu -ArgumentList $args -PassThru -WindowStyle Hidden

$script:qmp = $null
function Send-Qmp([string]$cmd) {
    if (-not $script:qmp) {
        $c = New-Object System.Net.Sockets.TcpClient("127.0.0.1", $port)
        $s = $c.GetStream()
        $script:qmp = @{ c = $c; w = (New-Object System.IO.StreamWriter($s)); r = (New-Object System.IO.StreamReader($s)) }
        $null = $script:qmp.r.ReadLine()
        $script:qmp.w.WriteLine('{"execute":"qmp_capabilities"}'); $script:qmp.w.Flush(); $null = $script:qmp.r.ReadLine()
    }
    $script:qmp.w.WriteLine($cmd); $script:qmp.w.Flush()
    do { $line = $script:qmp.r.ReadLine() } while ($line -like '*"event"*')
}
function Send-Input([string]$events) { Send-Qmp "{`"execute`":`"input-send-event`",`"arguments`":{`"events`":[$events]}}"; Start-Sleep -Milliseconds 60 }
function Send-Button([bool]$down) { Send-Input "{`"type`":`"btn`",`"data`":{`"down`":$($down.ToString().ToLower()),`"button`":`"left`"}}" }
function Save-Shot([string]$name) {
    $abs = (Resolve-Path test-out).Path + "\$name.ppm"
    Send-Qmp "{`"execute`":`"screendump`",`"arguments`":{`"filename`":`"$($abs -replace '\\','\\\\')`"}}"
    Start-Sleep -Milliseconds 300
}

try {
    Start-Sleep -Seconds $Seconds
    foreach ($e in $Events) {
        $kind, $arg = $e -split ':', 2
        switch ($kind) {
            "key"      { Send-Qmp "{`"execute`":`"send-key`",`"arguments`":{`"keys`":[{`"type`":`"qcode`",`"data`":`"$arg`"}]}}"; Start-Sleep -Milliseconds 200 }
            "move"     {
                # PS/2 packets carry at most ~127 per axis, so send big moves in small steps.
                $dx, $dy = [int[]]($arg -split ',')
                $steps = [Math]::Max(1, [Math]::Ceiling([Math]::Max([Math]::Abs($dx), [Math]::Abs($dy)) / 40))
                $sx = 0; $sy = 0
                for ($i = 1; $i -le $steps; $i++) {
                    $nx = [int]($dx * $i / $steps) - $sx; $ny = [int]($dy * $i / $steps) - $sy
                    $sx += $nx; $sy += $ny
                    Send-Input "{`"type`":`"rel`",`"data`":{`"axis`":`"x`",`"value`":$nx}},{`"type`":`"rel`",`"data`":{`"axis`":`"y`",`"value`":$ny}}"
                }
                Start-Sleep -Milliseconds 200
            }
            "click"    { Send-Button $true; Send-Button $false; Start-Sleep -Milliseconds 200 }
            "dblclick" { Send-Button $true; Send-Button $false; Send-Button $true; Send-Button $false; Start-Sleep -Milliseconds 300 }
            "wheel"    {
                # wheel:down or wheel:up (one notch)
                Send-Input "{`"type`":`"btn`",`"data`":{`"down`":true,`"button`":`"wheel-$arg`"}}"
                Send-Input "{`"type`":`"btn`",`"data`":{`"down`":false,`"button`":`"wheel-$arg`"}}"
                Start-Sleep -Milliseconds 100
            }
            "plug"     { Send-Qmp "{`"execute`":`"device_add`",`"arguments`":{`"driver`":`"usb-storage`",`"bus`":`"xhci.0`",`"drive`":`"$arg`",`"id`":`"dev-$arg`"}}" }
            "unplug"   { Send-Qmp "{`"execute`":`"device_del`",`"arguments`":{`"id`":`"dev-$arg`"}}" }
            "down"     { Send-Button $true }
            "up"       { Send-Button $false }
            "wait"     { Start-Sleep -Milliseconds ([int]$arg) }
            "shot"     { Save-Shot $arg }
        }
    }
    Start-Sleep -Milliseconds 500
    Save-Shot "screen"
    $script:qmp.c.Close()
} finally {
    if (-not $p.HasExited) { Stop-Process -Id $p.Id -Force }
}

# Convert PPM screenshots -> PNG so they can be viewed easily.
Add-Type -AssemblyName System.Drawing
foreach ($ppm in Get-ChildItem test-out\*.ppm) {
    $b = [IO.File]::ReadAllBytes($ppm.FullName)
    $i = 0; $f = @()
    while ($f.Count -lt 4) { $t = ""; while ([char]$b[$i] -match '\s') { $i++ }; while (-not ([char]$b[$i] -match '\s')) { $t += [char]$b[$i]; $i++ }; $f += $t }
    $i++; $W = [int]$f[1]; $H = [int]$f[2]
    $bmp = New-Object System.Drawing.Bitmap($W, $H, [System.Drawing.Imaging.PixelFormat]::Format24bppRgb)
    $data = $bmp.LockBits((New-Object System.Drawing.Rectangle(0,0,$W,$H)), "WriteOnly", $bmp.PixelFormat)
    $row = New-Object byte[] ($data.Stride)
    for ($y = 0; $y -lt $H; $y++) {
        for ($x = 0; $x -lt $W; $x++) { $o = $i + ($y*$W+$x)*3; $row[$x*3] = $b[$o+2]; $row[$x*3+1] = $b[$o+1]; $row[$x*3+2] = $b[$o] }
        [Runtime.InteropServices.Marshal]::Copy($row, 0, [IntPtr]($data.Scan0.ToInt64() + $y*$data.Stride), $data.Stride)
    }
    $bmp.UnlockBits($data); $bmp.Save([IO.Path]::ChangeExtension($ppm.FullName, ".png")); $bmp.Dispose()
    Remove-Item $ppm.FullName
}

$text = if (Test-Path $log) { Get-Content $log -Raw -Encoding UTF8 } else { "" }
Write-Host "---- serial log ----"; Write-Host $text
$fail = @($Expect | Where-Object { $text -notlike "*$_*" })
if ($text -like "*KERNEL PANIC*") { $fail += "no kernel panic" }
if ($text -like "*X64 Exception*") { $fail += "no firmware crash" }
foreach ($pair in $script:diskChecks) {
    Start-Sleep -Milliseconds 300   # let QEMU release the file
    if ((Get-FileHash $pair[0]).Hash -ne (Get-FileHash $pair[1]).Hash) { $fail += "USB disk $($pair[0]) left unmodified" }
    else { Write-Host "USB disk $($pair[0]): unchanged (read-only OK)" }
}
if ($fail.Count) { Write-Host "TEST FAILED, missing: $($fail -join ', ')" -ForegroundColor Red; exit 1 }
Write-Host "TEST PASSED" -ForegroundColor Green
