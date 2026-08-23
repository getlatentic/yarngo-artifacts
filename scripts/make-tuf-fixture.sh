#!/usr/bin/env bash
# Builds the TUF repositories the runtime-trust tests load.
#
# Run this only to regenerate the fixture; the output is committed, so the tests
# need tough alone and not tuftool. Keys are written to a temporary directory and
# discarded — nothing here signs anything real, and a private key in the tree
# would invite someone to believe otherwise.
#
#   scripts/make-tuf-fixture.sh
set -euo pipefail

command -v tuftool >/dev/null || { echo "tuftool not installed: cargo install tuftool --locked" >&2; exit 1; }

root=$(cd "$(dirname "$0")/.." && pwd)
out="$root/crates/speech-engine/tests/fixtures/tuf"
keys=$(mktemp -d)
trap 'rm -rf "$keys"' EXIT

# Far enough away that the fixture does not rot, close enough to be a real date.
far='2125-01-01T00:00:00Z'
# Already gone, so that enforcing expiry is observable rather than asserted.
past='2020-01-01T00:00:00Z'

# A repository, signed by its own key per role, exactly as the real one would be.
# $1 output name  $2 target contents  $3 expiry  $4 targets version
build() {
  local name=$1 body=$2 expires=$3 version=$4
  local dir="$keys/$name" ; mkdir -p "$dir/keys" "$dir/in"
  printf '%s' "$body" > "$dir/in/yarngo-runtime-spike.tar.gz"

  tuftool root init "$dir/root.json"
  tuftool root expire "$dir/root.json" "$far"
  for role in root targets snapshot timestamp; do
    tuftool root set-threshold "$dir/root.json" "$role" 1
    # One key per role: losing the timestamp key must not mean losing the
    # ability to say which targets are ours.
    tuftool root gen-rsa-key "$dir/root.json" "$dir/keys/$role.pem" --role "$role"
  done
  tuftool root sign "$dir/root.json" -k "$dir/keys/root.pem"

  tuftool create \
    --root "$dir/root.json" \
    -k "$dir/keys/targets.pem" -k "$dir/keys/snapshot.pem" -k "$dir/keys/timestamp.pem" \
    --add-targets "$dir/in" \
    --targets-expires "$expires" --targets-version "$version" \
    --snapshot-expires "$expires" --snapshot-version "$version" \
    --timestamp-expires "$expires" --timestamp-version "$version" \
    --outdir "$out/$name"
  cp "$dir/root.json" "$out/$name/root.json"

  # tuftool links a target back to where it was read from, and where it was read
  # from is about to be deleted. Take the bytes.
  find "$out/$name/targets" -type l | while read -r link; do
    cp -L "$link" "$link.real" && mv -f "$link.real" "$link"
  done
}

rm -rf "$out" && mkdir -p "$out"

# What the application would ship and serve.
build repo 'a runtime archive, as far as this test is concerned' "$far" 1
# Someone else's repository, correctly signed — by keys we never trusted.
build impostor 'a runtime archive, as far as this test is concerned' "$far" 1
# Correctly signed, and stale.
build stale 'a runtime archive, as far as this test is concerned' "$past" 1

echo "wrote $out"
find "$out" -type f | sed "s|$out/|  |" | sort
