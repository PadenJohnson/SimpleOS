#!/usr/bin/env bash
# Publish the USB image on GitHub, as the release "latest" (its files replaced each time).
# Run by the pre-push hook (.githooks/pre-push) after tools/make-usb.sh; needs the gh CLI,
# logged in (gh auth login).
#
#   tools/publish-usb.sh upload <commit>   upload dist/ (built from <commit>) to the release
#   tools/publish-usb.sh retag <commit>    once <commit> is on GitHub, point the release's tag at it
set -euo pipefail
cd "$(dirname "$0")/.."

action=${1:?usage: publish-usb.sh upload|retag <commit>}
sha=${2:?usage: publish-usb.sh upload|retag <commit>}
tag=latest
# owner/name from the origin remote (https://github.com/o/r(.git) or git@github.com:o/r(.git)).
repo=$(git remote get-url origin | sed -E 's#^(https://github.com/|git@github.com:)##; s#\.git$##')

case "$action" in
upload)
  [ "$(cat dist/BUILD 2>/dev/null)" = "$sha" ] || {
    echo "dist/solon.img wasn't built from $sha with a clean tree (dist/BUILD: $(cat dist/BUILD 2>/dev/null || echo none)). Run tools/make-usb.sh." >&2
    exit 1
  }
  (cd dist && rm -f solon-usb.zip && zip -qr solon-usb.zip solon-usb && sha256sum solon.img solon-usb.zip > SHA256SUMS.txt)
  short=${sha:0:7}
  notes=$(cat <<EOF
The newest Solon for a real PC, built from commit $short ($(git log -1 --format=%cs "$sha")): $(git log -1 --format=%s "$sha")

- **solon.img**: write it to a USB stick with Rufus (DD Image mode) or balenaEtcher, then boot the PC from the stick. Works on UEFI and old BIOS PCs, and keeps your photo library on the stick.
- **solon-usb.zip**: or unzip it onto a stick formatted as FAT32 (UEFI PCs only).
- **SHA256SUMS.txt**: checksums of the two files.

Secure Boot can stay on: the first time, approve Solon once in the blue shim screen (see docs/real-pc-guide.md).

This release is replaced automatically every time new work is pushed.
EOF
)
  if ! gh release view "$tag" -R "$repo" >/dev/null 2>&1; then
    # The new commit isn't on GitHub yet; start the tag at main and move it with "retag".
    gh release create "$tag" -R "$repo" --target main --title "Solon USB image (latest)" --notes "$notes" --latest
  else
    gh release edit "$tag" -R "$repo" --notes "$notes"
  fi
  gh release upload "$tag" -R "$repo" --clobber dist/solon.img dist/solon-usb.zip dist/SHA256SUMS.txt
  echo "Published: https://github.com/$repo/releases/tag/$tag"
  ;;
retag)
  # Wait (up to 10 minutes) for the push to land, then move the tag.
  for _ in $(seq 120); do
    if git ls-remote origin refs/heads/main | grep -q "^$sha"; then
      gh api -X PATCH "repos/$repo/git/refs/tags/$tag" -f sha="$sha" -F force=true >/dev/null
      echo "tag $tag -> $sha"
      exit 0
    fi
    sleep 5
  done
  echo "$sha didn't reach GitHub within 10 minutes; tag $tag not moved" >&2
  exit 1
  ;;
*) echo "usage: publish-usb.sh upload|retag <commit>" >&2; exit 2 ;;
esac
