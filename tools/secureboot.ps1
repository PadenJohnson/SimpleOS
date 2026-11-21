# Secure Boot for Solon's boot loader.
#
# Solon starts under Secure Boot through shim (Microsoft-signed, from Fedora; in
# secureboot\), which starts Limine. Shim only starts programs that carry an ".sbat"
# section, so Limine gets one here.
#
# The first time a PC starts Solon with Secure Boot on, shim's key manager asks once to add
# the fingerprint (hash) of Solon's boot loader: "Enroll hash from disk", then pick
# SOLON-SECURE-BOOT.efi on the stick. (Enrolling a signing key instead doesn't work: shim
# then requires the loader to have the kernel checked by shim, which Limine doesn't do, and
# halts with "Bootloader has not verified loaded image".) The fingerprint only changes when
# Limine itself is updated, not when Solon is.
#
#   . .\tools\secureboot.ps1 ; Invoke-SolonSecureLoader <limine BOOTX64.EFI> <out.efi> <mkimg>

$script:SolonRoot = Split-Path $PSScriptRoot

function Invoke-SolonSecureLoader([string]$In, [string]$Out, [string]$MkImg) {
    & $MkImg add-sbat $In $Out (Join-Path $SolonRoot "secureboot\sbat.csv")
    if ($LASTEXITCODE -ne 0) { throw "adding the SBAT section failed" }
}
