/*
 * A laptop battery for QEMU, which has none: `./test.sh --acpi-table
 * tools/acpitest/tests/qemu/battery.aml` adds it as an SSDT. It runs down as time passes
 * (the AML timer), from 100% at start to 2% about a minute in; then the charger is plugged in
 * and it charges, so the taskbar's battery, the low-battery warnings, the charger notice and
 * Settings > This PC can be seen.
 * Build with `iasl battery.asl` (Debian: acpica-tools).
 */
DefinitionBlock ("", "SSDT", 2, "SOLON", "BATTERY", 1)
{
    Scope (\_SB)
    {
        Device (BAT0)
        {
            Name (_HID, EisaId ("PNP0C0A"))
            Name (_UID, One)
            Method (_STA, 0, NotSerialized) { Return (0x1F) }
            // mWh: designed for 56 Wh, holds 50 Wh now.
            Name (BIXP, Package (0x15)
            {
                One, Zero, 56000, 50000, One, 11400, 5000, 1000, 312, 50000,
                0xFFFFFFFF, 0xFFFFFFFF, 1000, 500, 100, 100, "SOLON-42", "0042", "LION", "Solon",
                Zero
            })
            Method (_BIX, 0, NotSerialized) { Return (BIXP) }
            Method (_BST, 0, NotSerialized)
            {
                // Seconds since start (the timer counts 100 ns): 800 mWh go each second until
                // 62 s, then the charger is in and 800 mWh come back each second.
                Local0 = (Timer / 10000000)
                Name (BSTP, Package (0x04) { One, 9000, 50000, 11800 })
                If ((Local0 < 62))
                {
                    Local1 = (Local0 * 800)
                    If ((Local1 > 49000)) { Local1 = 49000 }
                    BSTP [0x02] = (50000 - Local1)
                }
                Else
                {
                    Local1 = (1000 + ((Local0 - 62) * 800))
                    If ((Local1 > 50000)) { Local1 = 50000 }
                    BSTP [Zero] = 0x02
                    BSTP [0x02] = Local1
                }
                Return (BSTP)
            }
        }

        Device (AC)
        {
            Name (_HID, "ACPI0003")
            Method (_PSR, 0, NotSerialized)
            {
                If (((Timer / 10000000) < 62)) { Return (Zero) }
                Return (One)
            }
        }
    }
}
