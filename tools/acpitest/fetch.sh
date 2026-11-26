#!/usr/bin/env bash
# Fetch laptops' ACPI tables from github.com/linuxhw/ACPI (dumps people have shared with the
# Linux hardware database) into folders acpitest can read, one per machine.
# Usage: fetch.sh <out folder> <path in that repository>...
#   e.g. fetch.sh dumps "Notebook/Dell/Latitude/Latitude E6530/8B5EC6878A4B.bin"
# Needs acpixtract (Debian: acpica-tools).
set -euo pipefail
out=$1; shift
mkdir -p "$out"
for p in "$@"; do
  name=$(echo "$p" | sed 's|^Notebook/||; s|\.bin$||; s|[^A-Za-z0-9._-]|_|g')
  dir="$out/$name"
  [ -e "$dir/dsdt.dat" ] && continue
  mkdir -p "$dir"
  url="https://raw.githubusercontent.com/linuxhw/ACPI/master/$(echo "$p" | sed 's| |%20|g')"
  if ! curl -sfL -o "$dir/dump.txt" "$url"; then echo "can't fetch $p"; rm -rf "$dir"; continue; fi
  (cd "$dir" && acpixtract -a dump.txt >/dev/null 2>&1 || true; rm -f dump.txt)
  [ -e "$dir/dsdt.dat" ] || { echo "no DSDT in $p"; rm -rf "$dir"; }
done
