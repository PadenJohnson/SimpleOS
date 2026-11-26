#!/usr/bin/env bash
# Assemble the UEFI boot folder (./esp) with the kernel and photos (bash version of make-esp.ps1).
# Photos are loaded into memory by the bootloader (Limine modules); songs (--music) are copied
# into a "music" folder, which the Music app reads like the demo songs on a real stick.
# Usage: tools/make-esp.sh [--profile debug|release] [--photos DIR]... [--music DIR] [--cmdline TEXT] [--resolution WxH]
set -euo pipefail
cd "$(dirname "$0")/.."

profile=debug cmdline="" resolution="" music=""
photos=()
while [ $# -gt 0 ]; do
  case "$1" in
    --profile) profile=$2; shift 2 ;;
    --photos) photos+=("$2"); shift 2 ;;
    --music) music=$2; shift 2 ;;
    --cmdline) cmdline=$2; shift 2 ;;
    --resolution) resolution=$2; shift 2 ;;
    *) echo "make-esp.sh: unknown option $1" >&2; exit 2 ;;
  esac
done

if [ ! -f limine/BOOTX64.EFI ]; then
  echo "Limine binaries not found. Run: git clone https://github.com/limine-bootloader/limine.git --branch=v9.x-binary --depth=1" >&2
  exit 1
fi
[ ${#photos[@]} -eq 0 ] && photos=(samples)

rm -rf esp
mkdir -p esp/EFI/BOOT esp/boot esp/photos
cp limine/BOOTX64.EFI esp/EFI/BOOT/BOOTX64.EFI
cp "target/x86_64-solon/$profile/solon" esp/boot/kernel

conf=$(cat limine.conf)$'\n'   # $(...) trims trailing newlines, like TrimEnd()
[ -n "$cmdline" ] && conf+="    cmdline: $cmdline"$'\n'
[ -n "$resolution" ] && conf+="    resolution: $resolution"$'\n'
count=0
shopt -s nocasematch
ext_re='\.(jpe?g|png|gif|bmp|tiff?|webp|heic|heif|psd|tga|ico|ppm|pgm|pbm|pam|dng|cr2|cr3|nef|arw|orf|rw2|raf|pef|srw|avi|wav|mp4|mov)$'
for dir in "${photos[@]}"; do
  if [ ! -d "$dir" ]; then echo "Photo folder not found: $dir"; continue; fi
  while IFS= read -r -d '' f; do
    base=${f##*/}
    [[ $base =~ $ext_re ]] || continue
    name=$(printf '%s' "$base" | sed 's/[^A-Za-z0-9._-]/_/g')   # Limine paths: keep names simple
    cp "$f" "esp/photos/$name"
    conf+="    module_path: boot():/photos/$name"$'\n'
    count=$((count + 1))
  done < <(find "$dir" -maxdepth 1 -type f -print0 | LC_ALL=C sort -zf)
done
shopt -u nocasematch
printf '%s' "$conf" > esp/boot/limine.conf
songs=0
if [ -n "$music" ]; then
  if [ -d "$music" ]; then
    mkdir -p esp/music
    cp "$music"/* esp/music/
    songs=$(find esp/music -type f | wc -l)
  else
    echo "Music folder not found: $music"
  fi
fi
echo "esp ready: kernel + $count photo(s) + $songs file(s) in music"
