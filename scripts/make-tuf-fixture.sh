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

# A minimal engine that genuinely answers the handshake, so "install, start it
# from its own directory, verify what it says" can run against a fixture.
# $1 the version it claims  $2 the methods it answers with (JSON array)
engine_speaking() {
  cat <<PYEOF
# the published engine
import json, sys

for line in sys.stdin:
    try:
        message = json.loads(line)
    except ValueError:
        continue
    reply = {"jsonrpc": "2.0", "id": message.get("id")}
    if message.get("method") == "initialize":
        reply["result"] = {
            "protocol": "yarngo-engine", "version": $1,
            "backend": "fixture", "conditioning_eviction": "none",
            "methods": $2,
        }
    else:
        reply["result"] = {}
    print(json.dumps(reply), flush=True)
    if message.get("method") == "shutdown":
        break
PYEOF
}

# A repository, signed by its own key per role, exactly as the real one would be.
# $1 output name  $2 target contents  $3 expiry  $4 targets version
build() {
  local name=$1 body=$2 expires=$3 version=$4
  local engine_version=${5:-1}
  local engine_methods=${6:-'["conditioning.prepare", "synthesis.generate", "ping"]'}
  local dir="$keys/$name" ; mkdir -p "$dir/keys" "$dir/in"
  printf '%s' "$body" > "$dir/in/yarngo-runtime-spike.tar.gz"

  # The catalogue is a target like any other, so what it says about versions is
  # covered by the same signatures as the archives it names. It carries no
  # digests and no URLs: where a target lives and what it hashes to is already
  # the repository's to say.
  cat > "$dir/in/catalogue.json" <<'JSON'
{
  "schema": 1,
  "runtimes": {
    "mlx": [
      { "version": "1.0.0", "min_app_version": "0.1.0-alpha.1", "engine_api": 1,
        "lock": "mlx-1.0.0.uv.lock", "pyproject": "mlx-1.0.0.pyproject.toml",
        "engine": "mlx-1.0.0.engine.tar.gz" },
      { "version": "2.0.0", "min_app_version": "9.0.0", "engine_api": 1,
        "lock": "mlx-2.0.0.uv.lock", "pyproject": "mlx-2.0.0.pyproject.toml" }
    ]
  }
}
JSON

  # The release the catalogue points at, so the whole path can be walked: read
  # the catalogue, choose, fetch what it named. The engine is a real one as far
  # as the wire is concerned — it answers the handshake — so an install test
  # can carry a fixture release every step of the way to "active".
  mkdir -p "$dir/in/engine"
  printf 'version = 1\nrequires-python = ">=3.13"\n' > "$dir/in/mlx-1.0.0.uv.lock"
  printf '[project]\nname = "mlx-runtime"\nversion = "1.0.0"\n' \
    > "$dir/in/mlx-1.0.0.pyproject.toml"
  engine_speaking "$engine_version" "$engine_methods" > "$dir/in/engine/engine.py"
  tar -czf "$dir/in/mlx-1.0.0.engine.tar.gz" -C "$dir/in/engine" engine.py
  rm -rf "$dir/in/engine"

  if [ ! -f "$dir/root.json" ]; then
    tuftool root init "$dir/root.json"
    tuftool root expire "$dir/root.json" "$far"
    for role in root targets snapshot timestamp; do
      tuftool root set-threshold "$dir/root.json" "$role" 1
      # One key per role: losing the timestamp key must not mean losing the
      # ability to say which targets are ours.
      tuftool root gen-rsa-key "$dir/root.json" "$dir/keys/$role.pem" --role "$role"
    done
    tuftool root sign "$dir/root.json" -k "$dir/keys/root.pem"
  fi

  tuftool create \
    --root "$dir/root.json" \
    -k "$dir/keys/targets.pem" -k "$dir/keys/snapshot.pem" -k "$dir/keys/timestamp.pem" \
    --add-targets "$dir/in" \
    --targets-expires "$expires" --targets-version "$version" \
    --snapshot-expires "$expires" --snapshot-version "$version" \
    --timestamp-expires "$expires" --timestamp-version "$version" \
    --outdir "$out/$name.new"
  rm -rf "$out/$name" && mv "$out/$name.new" "$out/$name"
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

# The same publisher's earlier metadata, kept so that being served last week's
# repository is something a test can do. Built from repo's keys, which is the
# point: replaying an old snapshot needs no keys at all.
cp -R "$out/repo" "$out/rollback"
build repo 'a runtime archive, as far as this test is concerned' "$far" 2

# Two repositories publishing runtimes that install cleanly and then disqualify
# themselves at the handshake: one answering a version this build does not
# speak, one answering without the methods that make it a speech runtime. The
# install path must refuse both without touching whatever was answering before.
build newapi 'a runtime archive, as far as this test is concerned' "$far" 1 2
build mute 'a runtime archive, as far as this test is concerned' "$far" 1 1 '["ping"]' 

echo "wrote $out"
find "$out" -type f | sed "s|$out/|  |" | sort
