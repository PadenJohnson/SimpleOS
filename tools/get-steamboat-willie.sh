#!/usr/bin/env bash
# Steamboat Willie (1928), for trying Solon's Videos app: the 1080p VP9 + Opus WebM that
# Wikimedia Commons makes from its copy of the film. Public domain in the US since 1 January
# 2024; still under copyright in some countries (Germany, for one). Mickey Mouse is a Disney
# trademark: fine for watching the film, not for Solon's own name or pictures.
#
#   tools/get-steamboat-willie.sh               download it (once) to ~/.cache/solon-videos
#   tools/get-steamboat-willie.sh /media/SOLON  and copy it into that Solon storage's Videos
#                                               folder (nothing already there is replaced)
set -euo pipefail

url='https://upload.wikimedia.org/wikipedia/commons/transcoded/5/5f/Steamboat_Willie_%281928%29_by_Walt_Disney.webm/Steamboat_Willie_%281928%29_by_Walt_Disney.webm.1080p.vp9.webm'
# The copy Solon was tested with (1296x1080 VP9, Opus, 7:40, 140 MB). Commons can encode its
# copies again, so a different file is reported, not refused.
tested=3941f53021f7c51f48ef9aca517d5b9ce3a611c88a7f8041016300f177917d2b
name='Steamboat Willie (1928).webm'
dir=${XDG_CACHE_HOME:-$HOME/.cache}/solon-videos
file=$dir/$name

mkdir -p "$dir"
if [ ! -s "$file" ]; then
  echo "Downloading $name (1080p, about 140 MB) from Wikimedia Commons..."
  curl -fL --retry 3 -A "SolonSampleFetch/1.0" -o "$file.part" "$url"
  mv "$file.part" "$file"
fi
if [ "$(sha256sum "$file" | cut -d' ' -f1)" = "$tested" ]; then
  echo "$file: the copy Solon was tested with"
else
  echo "$file: not the copy Solon was tested with (Commons may have encoded it again); it should still play"
fi

if [ $# -ge 1 ]; then
  storage=$1
  if [ ! -d "$storage" ]; then
    echo "$storage isn't a folder (mount the stick's SOLON partition first)" >&2
    exit 1
  fi
  mkdir -p "$storage/Videos"
  if [ -e "$storage/Videos/$name" ]; then
    echo "$storage/Videos/$name is already there: left as it is"
  else
    cp "$file" "$storage/Videos/$name.part"
    mv "$storage/Videos/$name.part" "$storage/Videos/$name"
    sync
    echo "Copied to $storage/Videos/$name"
  fi
fi
