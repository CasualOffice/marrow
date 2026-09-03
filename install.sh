#!/bin/bash
#
# Install Marrow.
#
#   curl -fsSL https://raw.githubusercontent.com/CasualOffice/marrow/main/install.sh | bash
#
# **Read this before you pipe it into a shell.** It is short on purpose so that
# reading it is realistic, and everything it does is listed here:
#
#   1. Refuses to run on an Intel Mac, where the binary cannot work at all.
#   2. Asks GitHub for the latest release and downloads the .dmg and SHA256SUMS.
#   3. **Verifies the download against the published checksum**, and stops if it
#      does not match.
#   4. Copies Marrow.app into /Applications.
#   5. Removes `com.apple.quarantine` from that one path.
#   6. Ejects the disk image.
#
# Step 5 is the reason this exists. The build is signed ad-hoc rather than
# notarised — notarisation needs a paid Apple Developer account and this is a
# personal project — so Gatekeeper refuses it with "Apple could not verify
# 'Marrow' is free of malware". That refusal is Gatekeeper working correctly,
# and the honest thing is not to pretend otherwise: what makes clearing the flag
# defensible here is step 3, which this script does and a person following
# instructions almost never does.
#
# If you would rather not run this: download the .dmg from the releases page,
# drag the app to Applications, and then run
#   xattr -dr com.apple.quarantine /Applications/Marrow.app
# which is the same thing with the checksum left to you.

set -euo pipefail

REPO="CasualOffice/marrow"
APP="/Applications/Marrow.app"

say() { printf '  %s\n' "$*"; }
die() { printf '\n  %s\n\n' "$*" >&2; exit 1; }

printf '\n  Marrow\n\n'

# ── 1. This binary is arm64 only ─────────────────────────────────────────────
# Not a preference: local inference is MLX on Apple Silicon and there is no CPU
# path. Saying so here beats a downloaded app that cannot launch and a dialog
# that does not explain why.
[ "$(uname -s)" = "Darwin" ] || die "Marrow is macOS only."
if [ "$(uname -m)" != "arm64" ]; then
  die "This Mac has an Intel processor, and Marrow ships an Apple Silicon build only.
  There is no workaround — Rosetta translates Intel to ARM, not the other way."
fi

work="$(mktemp -d)"
trap 'rm -rf "$work"; [ -d "$work/mnt" ] && hdiutil detach "$work/mnt" -quiet 2>/dev/null || true' EXIT

# ── 2. Find the latest release ───────────────────────────────────────────────
say "Looking up the latest release…"
api="https://api.github.com/repos/$REPO/releases/latest"
json="$(curl -fsSL "$api")" || die "Could not reach GitHub."

dmg_url="$(printf '%s' "$json" | grep -o '"browser_download_url": *"[^"]*\.dmg"' | head -1 | sed 's/.*"\(https[^"]*\)"/\1/')"
sums_url="$(printf '%s' "$json" | grep -o '"browser_download_url": *"[^"]*SHA256SUMS.txt"' | head -1 | sed 's/.*"\(https[^"]*\)"/\1/')"
tag="$(printf '%s' "$json" | grep -o '"tag_name": *"[^"]*"' | head -1 | sed 's/.*"\([^"]*\)"$/\1/')"

[ -n "$dmg_url" ] || die "That release has no .dmg attached to it."
say "Found $tag"

# ── 3. Download, and verify before trusting ──────────────────────────────────
dmg="$work/$(basename "$dmg_url")"
say "Downloading $(basename "$dmg_url")…"
curl -fsSL --progress-bar "$dmg_url" -o "$dmg" || die "Download failed."

if [ -n "$sums_url" ]; then
  curl -fsSL "$sums_url" -o "$work/SHA256SUMS.txt" || die "Could not fetch the checksums."
  want="$(grep " $(basename "$dmg")\$" "$work/SHA256SUMS.txt" | awk '{print $1}')"
  got="$(shasum -a 256 "$dmg" | awk '{print $1}')"
  if [ -z "$want" ]; then
    die "The release publishes checksums but none for $(basename "$dmg"). Stopping rather than installing something unverified."
  fi
  if [ "$want" != "$got" ]; then
    die "Checksum mismatch. Nothing was installed.
    expected $want
    got      $got"
  fi
  say "Checksum verified."
else
  # Never silently. Clearing the quarantine flag is only defensible because the
  # bytes were checked, so a release with no checksums does not get that step
  # taken on the user's behalf without them knowing.
  say "WARNING: this release publishes no checksums, so the download was not verified."
fi

# ── 4. Install ───────────────────────────────────────────────────────────────
mkdir -p "$work/mnt"
hdiutil attach -nobrowse -quiet -mountpoint "$work/mnt" "$dmg" || die "Could not open the disk image."
src="$(find "$work/mnt" -maxdepth 1 -name '*.app' | head -1)"
[ -n "$src" ] || die "No application inside the disk image."

if [ -d "$APP" ]; then
  say "Replacing the copy already in /Applications…"
  rm -rf "$APP"
fi
# `ditto` rather than `cp`: it preserves the extended attributes and symlinks a
# signed bundle is made of, and a bundle copied with `cp -r` can fail to verify.
ditto "$src" "$APP" || die "Could not copy into /Applications. Try again with sudo."
hdiutil detach "$work/mnt" -quiet || true

# ── 5. The step this script exists for ───────────────────────────────────────
xattr -dr com.apple.quarantine "$APP" 2>/dev/null || true

if codesign --verify --deep --strict "$APP" 2>/dev/null; then
  say "Signature verified."
else
  say "WARNING: the installed bundle does not verify. Do not open it; report this."
fi

printf '\n  Installed %s to %s\n' "$tag" "$APP"
printf '  Open it from Applications, or run:  open -a Marrow\n\n'
