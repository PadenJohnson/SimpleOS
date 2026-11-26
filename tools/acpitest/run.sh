#!/usr/bin/env bash
# Build acpitest and run it on each folder given, each in its own process with a time limit
# (firmware code can wait forever on the pretend hardware). Cargo is started from outside
# Solon's folder so Solon's own build settings (.cargo/config.toml: its target and build-std)
# don't apply to this host tool.
set -uo pipefail
here=$(cd "$(dirname "$0")" && pwd)
(cd / && cargo +nightly build -q --release --manifest-path "$here/Cargo.toml") || exit 1
bin="$here/target/release/acpitest"
ok=0 bad=0
for d in "$@"; do
  out=$(timeout 60 "$bin" "$d" 2>&1); code=$?
  [ $code = 124 ] && out="$out"$'\n'"  HANG: still running after 60 s"
  echo "$out" | grep -v '^[0-9]* ok, [0-9]* failed$'
  if echo "$out" | grep -q '^  ok:'; then ok=$((ok + 1)); else bad=$((bad + 1)); fi
done
echo "$ok ok, $bad failed"
