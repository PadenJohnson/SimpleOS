# The acpi crate, as Solon uses it

This is [rust-osdev/acpi](https://github.com/rust-osdev/acpi) at commit 65c203d2
(7.0.0-pre2 and the fixes after it, 2026-11-23), MIT/Apache-2.0 (LICENCE-MIT, LICENCE-APACHE).
Solon uses its table parsing and AML interpreter to run the firmware's battery code
(`src/aml.rs`, `src/battery.rs`). The crate's tests and tools aren't copied.

Running it over 90 laptops' ACPI tables with `tools/acpitest` turned up the problems below;
each change is marked `Solon:` in the source. They are worth sending upstream.

- **Embedded controller access.** An `EmbeddedControl` operation region (and the other
  spaces served by a handler) went nowhere: the installed `RegionHandler` was never called
  ("Custom region handlers aren't actually supported yet"). Accesses now reach it, with the
  field's offset added to the region's base, since the trait passes only the region.
- **`Not`, `FindSetLeftBit`, `FindSetRightBit`, `ToBCD`, `FromBCD`** take a result target
  (`Not (Operand, Target)`); the code expected one argument, so every use failed. `Not` was
  also a logical not (0 or ones) instead of the bitwise complement.
- **`Acquire` has a value**: zero when it got the mutex, ones on a timeout. Without it,
  `Local0 = Acquire (MUTX, 1000)` left `Local0` uninitialized (Huawei laptops).
- **A method that ends without `Return`** gave its caller nothing, so in `X = Method ()` the
  store took the caller's next opcode as its value and the method went wrong from there
  (AMD's ALIB, called from ASUS laptops' `_PSR`). It now gives zero, as Windows does.
- **`ToString`** failed on any byte that isn't UTF-8 (an EC hands back 0xFF for an empty
  name); bytes are taken as they are, as ACPICA does.
- **`Mid`** didn't look through references and panicked when index + length passed the end.
- **`Alias`** looked its source up without the namespace search rules (a name may live in an
  enclosing scope) and failed the whole table when the source was missing; it now searches,
  and skips a missing one with a warning (Dell Latitude E6530).
- **Shifts** by 64 or more give 0 (they wrapped), and arithmetic results are cut to the
  table's integer size (32 bits for revision 1 tables).
- **Panics on real tables** became errors or warnings: a `_STA` that isn't a plain integer,
  a package with more initializers than its declared size (allowed), a `Connect` entry in a
  field list (GPIO and serial-bus connections; skipped), and a block length past the end of
  its enclosing block (cut short).
- `#![allow(unstable_name_collisions)]`: newer nightly compilers warn about `bit_field`'s
  `set_bit` on every build.

Solon also guards against what's left: the interpreter runs on a worker core, and a panic
inside it returns to `aml.rs` (`guard`) instead of reaching the crash screen.
