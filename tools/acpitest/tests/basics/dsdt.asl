/*
 * acpitest's own machine: a battery and charger whose methods use the AML that real laptops'
 * battery code relies on, with answers known in advance (expected.txt). Build with
 * `iasl -f -oa dsdt.asl` (Debian: acpica-tools; -oa so the sums stay for the interpreter to do,
 * not folded by iasl; -f because NORT returning nothing is the point,
 * which iasl refuses); dsdt.aml is kept next to it.
 */
DefinitionBlock ("", "DSDT", 2, "SOLON", "ACPITEST", 1)
{
    Scope (\_SB)
    {
        Device (EC0)
        {
            Name (_HID, EisaId ("PNP0C09"))
            Name (ECOK, Zero)
            Name (BUF, ResourceTemplate ()
            {
                IO (Decode16, 0x0930, 0x0930, 0x00, 0x01)
                IO (Decode16, 0x0934, 0x0934, 0x00, 0x01)
            })
            Method (_CRS, 0, NotSerialized) { Return (BUF) }
            Method (_REG, 2, NotSerialized)
            {
                If (((Arg0 == 0x03) && (Arg1 == One))) { ECOK = One }
            }
            OperationRegion (ECOR, EmbeddedControl, Zero, 0xFF)
            Field (ECOR, ByteAcc, NoLock, Preserve)
            {
                Offset (0x10),
                ECB0, 8,        // the pretend EC holds 0x20 + 7 * 0x10 = 0x90 here
            }
        }

        Device (BAT0)
        {
            Name (_HID, EisaId ("PNP0C0A"))
            Name (_UID, One)
            Mutex (MUTX, 0)
            Method (_STA, 0, NotSerialized) { Return (0x1F) }
            Name (BIFP, Package (0x0D) { One, Zero, Zero, One, Zero, 400, 100, 10, 10, "", "", "", "" })

            Method (PUTV, 2, NotSerialized) { Arg1 = Arg0 }       // a store through RefOf
            Method (NORT, 0, NotSerialized) { Local0 = One }      // ends without Return

            Method (_BIF, 0, NotSerialized)
            {
                Local0 = Zero
                PUTV (4321, RefOf (Local0))
                BIFP [0x02] = (Local0 - 321)                       // last full: 4000
                Local1 = Acquire (MUTX, 100)                        // 0: got it
                Release (MUTX)
                Local2 = NORT ()                                    // 0
                BIFP [One] = (5000 + Local1 + Local2)               // design: 5000
                BIFP [0x04] = (11100 + (Not (0xFFFFFFFFFFFFFFFE) - One))  // design voltage: 11100
                BIFP [0x09] = ToString (Buffer () { 0x53, 0x6F, 0x6C, 0x6F, 0x6E, 0x00, 0x41 }, Ones)
                BIFP [0x0B] = Concatenate ("LI", "ON")
                BIFP [0x0C] = Mid ("xxMaker of Solon", 0x02, 100)
                Return (BIFP)
            }

            Method (_BST, 0, NotSerialized)
            {
                Name (BSTP, Package (0x04) {})
                If (!\_SB.EC0.ECOK) { Return (Package (0x04) { Zero, Zero, Zero, Zero }) }
                BSTP [Zero] = Not (0xFFFFFFFFFFFFFFFD)               // 2: charging
                BSTP [One] = (FromBCD (0x1500) + \_SB.EC0.ECB0 - 0x90)   // rate: 1500
                ToBCD (2000, Local3)
                BSTP [0x02] = (FromBCD (Local3) + FindSetRightBit (0x0100) - 9)  // remaining: 2000
                BSTP [0x03] = ((12000 >> 64) + 12000)                 // voltage: 12000
                Return (BSTP)
            }
        }

        Device (AC)
        {
            Name (_HID, "ACPI0003")
            Method (_PSR, 0, NotSerialized)
            {
                If ((Not (Zero) == Ones)) { Return (One) }
                Return (Zero)
            }
        }
    }
}
