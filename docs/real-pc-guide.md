# Running Solon on a real PC

This guide covers making a Solon USB stick, booting a PC from it, and what to report back.
Solon runs entirely from the stick. It doesn't change the PC's internal disk unless you
explicitly choose **Set up Solon storage** in Import.

## 1. Build the files

In the project folder:

```powershell
powershell -ExecutionPolicy Bypass -File .\tools\make-usb.ps1
```

This creates:

| Output | Use it for |
|---|---|
| `dist\solon.img` (66 MB) | **Recommended.** Written to a stick with Rufus or balenaEtcher. Boots on UEFI *and* old BIOS PCs, and keeps your photo library on the stick. |
| `dist\solon-usb\` | Files to copy onto a FAT32 stick. UEFI PCs only, no library on the stick. |

## 2. Make the stick

Use a stick you don't need: **writing the image erases it.** Any size works; 8 GB or more
leaves plenty of room for photos.

**Option A, disk image (recommended)**

1. Download [Rufus](https://rufus.ie) (Windows) or [balenaEtcher](https://etcher.balena.io).
2. Rufus: pick the stick under *Device*, click *SELECT* and choose `dist\solon.img`, click
   *START*. If it asks, choose **DD Image mode**. Etcher: *Flash from file* → `solon.img`
   → select the stick → *Flash*.
3. Windows may say the stick needs formatting afterwards. **Click Cancel.** That's normal,
   because Windows can't read Solon's library partition.

**Option B, copy files**

1. Format the stick as **FAT32** (File Explorer → right-click the stick → *Format*). Windows
   only offers FAT32 for sticks up to 32 GB.
2. Copy the *contents* of `dist\solon-usb\` (the `EFI` and `boot` folders) to the stick, or
   let the script do it: `.\tools\make-usb.ps1 -CopyTo E:` (use your stick's letter; it
   refuses anything that isn't a removable FAT32 drive, and never formats).

## 3. Boot the PC from the stick

1. Turn **Secure Boot off** in the PC's firmware setup (Solon's bootloader isn't signed by
   Microsoft). It's usually under *Security* or *Boot*.
2. Plug in the stick, power on, and press the **boot menu key** right away:

   | Brand | Boot menu key |
   |---|---|
   | Dell | F12 |
   | HP | F9 (or Esc, then F9) |
   | Lenovo | F12 (laptops: Fn+F12, or the small Novo button) |
   | ASUS | F8 (laptops: Esc) |
   | Acer | F12 (enable "F12 Boot Menu" in setup first) |
   | MSI | F11 |
   | Gigabyte | F12 |
   | ASRock | F11 |

3. Pick the entry that starts with **UEFI:** followed by your stick's name.

Solon's desktop should appear within a few seconds.

## 4. What to expect

- **First boot from the image (Option A):** Solon automatically sets up a "Solon Library"
  partition in the rest of the stick. Photos you import, and edited photos, are kept there
  and travel with the stick. Import shows it under *This computer → Solon storage*.
- **Option B sticks** have no library; you can view photos from other drives but not
  import or save edits, unless the PC has free internal disk space for *Set up Solon storage*.
- **Other USB drives and SD card readers are only written to when you ask.** Files can add
  copies to them (it never changes or deletes what is already there), and Disks can erase one
  after two warnings. Solon never erases an internal disk or its own stick.
- **Internal disks** (NVMe/SATA) are detected but not touched unless you confirm *Set up
  Solon storage*, which only ever uses empty space.
- To go back to Windows: shut down, remove the stick, and turn the PC on. Turn Secure Boot
  back on if you like.

## 5. Known limits on real hardware

- **Laptop touchpads** often don't work yet (many use an I2C connection Solon doesn't
  support). Plug in a USB mouse. Laptop keyboards usually work.
- **Very high-resolution screens** (4K) show everything small; there's no scaling yet.
- **PCs from before about 2012** (USB 2-only controllers) can boot Solon, but USB devices won't work.
- **No Wi-Fi or networking**: that's by design.
- Videos, documents and printing aren't built yet.

## 6. Reporting how it went

- **F12** shows Solon's diagnostics: the boot log, including which USB devices, disks and
  screen size were found. Take a photo of it.
- If Solon hits a problem it shows a **red "Solon stopped" screen** with details. Take a
  photo of that too.
- If nothing appears after choosing the stick, note the last thing on screen (a black
  screen, a Limine message, or the PC returning to its boot menu) and the PC's make and model.

To check the crash screen itself, add the line `    cmdline: crashtest` to the end of
`boot\limine.conf` on the stick: Solon will show the red screen two seconds after starting.
Remove the line afterwards.

## Phones

Plug the phone into the PC with its charging cable (a cable that only charges won't work).

- **Android:** unlock the phone, pull down the notification "Charging this device via USB",
  tap it and choose **File transfer** (sometimes called MTP). The phone then appears in Files
  and Import. If Solon shows "Unlock the phone..." first, do that and click the phone's row in
  Import.
- **iPhone:** unlock it and tap **Trust** (or **Allow**) when it asks about this computer.
  Solon sees the photos and videos in the camera roll (Apple doesn't let other computers see
  other files). If the iPhone is set to save photos as HEIC, keep *Settings > Photos >
  Transfer to Mac or PC* on **Automatic** so it sends ordinary JPEG photos.
- **Cameras:** turn the camera on; some need a "PC connection" or "MTP/PTP" USB setting.

In Import, open the phone and press **Import new photos & videos**: everything that wasn't
imported before is copied into Solon storage (a folder named with today's date and the phone).
Pressing it again later only copies the new ones. Solon never changes or deletes anything on
the phone.
