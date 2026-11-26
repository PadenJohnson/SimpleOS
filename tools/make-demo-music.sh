#!/usr/bin/env bash
# The demo songs the Music app shows on a new Solon stick: seven well-known public-domain
# recordings from Wikimedia Commons, one in each kind of file Solon plays (MP3, AAC in M4A,
# Ogg Vorbis, Opus, FLAC), with titles, artists, track numbers and a cover. The USB image puts
# them in the boot partition's "music" folder (make-usb.sh), and the installer copies that
# partition to the PC's disk, so they're there on first use either way.
#
#   tools/make-demo-music.sh <folder>    make the songs (once; kept in ~/.cache/solon-music)
#                                        and copy them into <folder>
#
# Needs curl and FFmpeg (with libmp3lame, libopus and libvorbis). The recordings are checked
# against the SHA-256 they had when they were chosen (2026-11-25); every one is in the public
# domain (US government bands, Musopen, recordings from 1923 and 1924) or CC0.
set -euo pipefail
cd "$(dirname "$0")/.."

out=${1:?usage: tools/make-demo-music.sh <folder>}
cache=${XDG_CACHE_HOME:-$HOME/.cache}/solon-music
src=$cache/src
set_dir=$cache/demo-3
mkdir -p "$src"

c='https://upload.wikimedia.org/wikipedia/commons'
# name|url|sha256
sources=(
  "entertainer.mp3|$c/b/bd/%22The_Entertainer%22_%281902%29%2C_by_Scott_Joplin.mp3|d666009b399f632b22da8783afd1545d1372fce2b0d782c5f4b8e882c875f146"
  "stars-and-stripes.mp3|$c/e/ed/Sousa%27s_%22The_Stars_and_Stripes_Forever%22_-_United_States_Marine_Band_%282017%29.mp3|9c4be041c99072277cf0c74674d9624f0d10189f18faee88169f0c79447afe50"
  "canon.mp3|$c/1/12/Canon_%282004%29_-_Strolling_Strings_-_United_States_Air_Force_Band.mp3|43d43266105d2f58a77b2a8756609e34310e48ce2faf2c90ca1e7390c8cdbbdd"
  "morning-mood.flac|$c/c/c2/Grieg_-_Peer_Gynt_Suite_No._1%2C_Op._46_-_I._Morning_Mood_%28Musopen_Symphony%29.flac|b667041ee4766e81b77e062e8fb1861c8aae418b4747fb0e3fec1dc0c80e3292"
  "mountain-king.flac|$c/8/84/Grieg_-_Peer_Gynt_Suite_No._1%2C_Op._46_-_IV._In_the_Hall_of_the_Mountain_King_%28Musopen_Symphony%29.flac|1c1be6b3e0042ee031cee42a8d151b04c78bfccf3126c4de6271e7b2fdcb6937"
  "minute-waltz.ogg|$c/9/9e/Minute_Waltz%2C_by_Chopin_-_Performed_by_Sergej_Rachmaninoff.ogg|4602b46e1e346dc32c4349cc25f38fd8100d66d574eebc91368b4043eb66e6c0"
  "rhapsody-1.mp3|$c/c/cb/Paul_Whiteman_Concert_Orchestra_-_Rhapsody_in_Blue_Part_01.mp3|d0c3c6b82d28582bdd14ede190351bff4d73a937838c15bb4717ece810ab992a"
  "rhapsody-2.mp3|$c/b/be/Paul_Whiteman_Concert_Orchestra_-_Rhapsody_in_Blue_Part_02.mp3|859396659bc4da1e66fdae3e7fd77953bb03266c96c93964de8dda4d105be40d"
)

for s in "${sources[@]}"; do
  IFS='|' read -r name url sum <<< "$s"
  f=$src/$name
  if [ ! -s "$f" ]; then
    echo "Downloading $name from Wikimedia Commons..."
    curl -fsL --retry 3 -A "SolonSampleFetch/1.0" -o "$f.part" "$url"
    mv "$f.part" "$f"
  fi
  if [ "$(sha256sum "$f" | cut -d' ' -f1)" != "$sum" ]; then
    echo "$f isn't the recording that was chosen (its SHA-256 differs): delete it to download it again" >&2
    exit 1
  fi
done

if [ ! -s "$set_dir/done" ]; then
  echo "Making the demo songs (FFmpeg)..."
  rm -rf "$set_dir"
  mkdir -p "$set_dir"
  tmp=$set_dir/tmp
  mkdir -p "$tmp"
  ff() { ffmpeg -nostdin -hide_banner -loglevel error -y "$@"; }
  album='Solon Demo Songs'

  # The cover (drawn for Solon: assets/demo-songs-cover.jpg).
  cp assets/demo-songs-cover.jpg "$set_dir/folder.jpg"

  # meta <title> <artist> <year> <track> <comment>
  meta() { printf '%s\n' -metadata "title=$1" -metadata "artist=$2" -metadata "album_artist=Various artists" -metadata "album=$album" -metadata "date=$3" -metadata "track=$4/7" -metadata "comment=$5"; }
  m() { mapfile -t M < <(meta "$@"); }

  # 1. MP3 (the original file, as it came, with new tags and the cover).
  m "The Entertainer" "Scott Joplin" "2018" 1 "Performed by James Brigham (2018); CC0"
  ff -i "$src/entertainer.mp3" -i "$set_dir/folder.jpg" -map 0:a -map 1 -c copy -id3v2_version 3 -map_metadata -1 "${M[@]}" \
     -metadata:s:v title="Album cover" -metadata:s:v comment="Cover (front)" "$set_dir/01 The Entertainer.mp3"
  # 2. AAC in M4A, with the cover.
  m "The Stars and Stripes Forever" "John Philip Sousa" "2017" 2 "United States Marine Band (2017); public domain"
  ff -i "$src/stars-and-stripes.mp3" -i "$set_dir/folder.jpg" -map 0:a -map 1 -c:a aac -b:a 112k -c:v copy -disposition:v attached_pic \
     -map_metadata -1 "${M[@]}" "$set_dir/02 The Stars and Stripes Forever.m4a"
  # 3. Ogg Vorbis (the cover comes from folder.jpg next to it).
  m "Canon in D" "Johann Pachelbel" "2004" 3 "United States Air Force Band, Strolling Strings (2004); public domain"
  ff -i "$src/canon.mp3" -map 0:a -c:a libvorbis -q:a 3 -map_metadata -1 "${M[@]}" "$set_dir/03 Canon in D.ogg"
  # 4. Opus.
  m "Morning Mood" "Edvard Grieg" "2012" 4 "Musopen Symphony (2012); public domain"
  ff -i "$src/morning-mood.flac" -map 0:a -c:a libopus -b:a 96k -map_metadata -1 "${M[@]}" "$set_dir/04 Morning Mood.opus"
  # 5. MP3 made from the lossless original.
  m "In the Hall of the Mountain King" "Edvard Grieg" "2012" 5 "Musopen Symphony (2012); public domain"
  ff -i "$src/mountain-king.flac" -i "$set_dir/folder.jpg" -map 0:a -map 1 -c:a libmp3lame -b:a 128k -c:v copy -id3v2_version 3 \
     -map_metadata -1 "${M[@]}" "$set_dir/05 In the Hall of the Mountain King.mp3"
  # 6. FLAC: a 1923 acoustic recording, mono (it has nothing above about 5 kHz, so 22 kHz keeps it all).
  m "Minute Waltz" "Frédéric Chopin" "1923" 6 "Sergei Rachmaninoff, piano (Victor, 1923); public domain"
  ff -i "$src/minute-waltz.ogg" -i "$set_dir/folder.jpg" -map 0:a -map 1 -ac 1 -ar 22050 -sample_fmt s16 -c:a flac -compression_level 8 \
     -c:v copy -disposition:v attached_pic -map_metadata -1 "${M[@]}" "$set_dir/06 Minute Waltz.flac"
  # 7. Opus, mono: the two sides of the 1924 record joined.
  m "Rhapsody in Blue" "George Gershwin" "1924" 7 "Paul Whiteman and his Concert Orchestra with George Gershwin, piano (Victor 55225, 1924); public domain"
  ff -i "$src/rhapsody-1.mp3" -i "$src/rhapsody-2.mp3" -filter_complex "[0:a][1:a]concat=n=2:v=0:a=1,pan=mono|c0=0.5*c0+0.5*c1[a]" -map "[a]" \
     -c:a libopus -b:a 48k -map_metadata -1 "${M[@]}" "$set_dir/07 Rhapsody in Blue.opus"

  cat > "$set_dir/CREDITS.txt" <<'EOF'
Solon Demo Songs - where they come from
=======================================
All seven recordings are in the public domain (or given to it, CC0). They come from
Wikimedia Commons (commons.wikimedia.org), and were made into these files with FFmpeg.

1. The Entertainer (Scott Joplin, 1902) - performed by James Brigham, 2018. CC0.
   File:"The Entertainer" (1902), by Scott Joplin.mp3
2. The Stars and Stripes Forever (John Philip Sousa, 1896) - United States Marine Band, 2017.
   Public domain (work of the US government).
   File:Sousa's "The Stars and Stripes Forever" - United States Marine Band (2017).mp3
3. Canon in D (Johann Pachelbel) - United States Air Force Band, Strolling Strings, 2004
   (arranged by Frank Hudson). Public domain (work of the US government).
   File:Canon (2004) - Strolling Strings - United States Air Force Band.mp3
4. Morning Mood (Edvard Grieg, Peer Gynt) - Musopen Symphony, 2012. Public domain (Musopen).
   File:Grieg - Peer Gynt Suite No. 1, Op. 46 - I. Morning Mood (Musopen Symphony).flac
5. In the Hall of the Mountain King (Edvard Grieg, Peer Gynt) - Musopen Symphony, 2012.
   Public domain (Musopen).
   File:Grieg - Peer Gynt Suite No. 1, Op. 46 - IV. In the Hall of the Mountain King (Musopen Symphony).flac
6. Minute Waltz (Frederic Chopin) - Sergei Rachmaninoff, piano, recorded 5 April 1923.
   Public domain (US recording published before 1925).
   File:Minute Waltz, by Chopin - Performed by Sergej Rachmaninoff.ogg
7. Rhapsody in Blue (George Gershwin, 1924) - Paul Whiteman and his Concert Orchestra with
   George Gershwin at the piano, recorded 10 June 1924 (Victor 55225, both sides).
   Public domain (US recording published before 1925).
   File:Paul Whiteman Concert Orchestra - Rhapsody in Blue Part 01.mp3, Part 02.mp3

The cover was made for Solon (public domain).
EOF
  sed -i 's/$/\r/' "$set_dir/CREDITS.txt"
  rm -rf "$tmp"
  date > "$set_dir/done"
fi

mkdir -p "$out"
for f in "$set_dir"/*; do
  [ "${f##*/}" = done ] && continue
  cp "$f" "$out/"
done
total=$(du -ch "$set_dir"/*.mp3 "$set_dir"/*.m4a "$set_dir"/*.ogg "$set_dir"/*.opus "$set_dir"/*.flac | tail -1 | cut -f1)
echo "Demo songs ($total) in $out"
