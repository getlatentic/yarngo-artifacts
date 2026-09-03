#!/usr/bin/env bash
# Sign and publish the runtimes this application will install.
#
#   scripts/tuf-repo.sh init             once, ever — creates the signing keys
#   scripts/tuf-repo.sh publish 1.2.3    per release — signs the current recipe
#   scripts/tuf-repo.sh status           what exists and what does not
#
# Two directories are involved and they could not be more different.
#
#   ~/.yarngo/tuf-keys    SECRET. The private keys. Whoever holds these decides
#                         what every installed copy of this application will
#                         download and run. Never in the repository, never in
#                         CI. Back them up somewhere you would back up a
#                         password, because losing them means no runtime can
#                         ever be published to anyone who has already installed
#                         the app.
#
#   dist/tuf              PUBLIC. The signed repository. Upload it as-is; every
#                         file in it is meant to be served to the internet.
#
# Both have defaults and neither needs to be typed. Override with
# YARNGO_TUF_KEYS and YARNGO_TUF_OUT if you keep them elsewhere.
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/.."

KEYS="${YARNGO_TUF_KEYS:-$HOME/.yarngo/tuf-keys}"
OUT="${YARNGO_TUF_OUT:-dist/tuf}"
ANCHOR="packaging/tuf/root.json"
# Where the application fetches from, so `publish` can say where to put this.
# Read from the source rather than repeated here, because two copies of an
# address that must match is one copy too many.
SERVED_AT="$(sed -n 's/^const REPOSITORY: &str = "\(.*\)";$/\1/p' \
  crates/speech-engine/src/published.rs | head -1)"
[ -n "$SERVED_AT" ] || {
  echo "Could not read the repository address from crates/speech-engine/src/published.rs" >&2
  exit 1
}

command -v tuftool >/dev/null || {
  echo "tuftool is not installed. Run: cargo install tuftool --locked" >&2
  exit 1
}

# Root expires furthest out because rotating it means shipping an application.
# Timestamp expires soonest because that is what freshness means: metadata
# nobody has re-signed lately stops being believed.
#
# A day or two would be the textbook timestamp window, and it assumes an online
# service holding the timestamp key alone. There is no such service here, and
# `tuftool` cannot re-sign the timestamp without the targets and snapshot keys
# as well — so automating a short window would mean an unattended machine
# holding every key, which is the thing the roles are separated to prevent.
# A month is what one person with the keys in their own hands can actually
# keep up with, and an expiry nobody meets is not a freshness guarantee, it is
# an outage with a delay on it. Shorten it when there is something to sign with
# that is not a laptop.
ROOT_EXPIRY='in 52 weeks'
ROLE_EXPIRY='in 26 weeks'
TIMESTAMP_EXPIRY='in 90 days'

sign_and_swap() {
  version="$1"
  staged="$2"
  # Monotonic across publishes, and kept with the keys rather than with the
  # output — the output is regenerated, and a metadata version that went
  # backwards would be read by every installed application as somebody
  # replaying an old repository at them.
  counter="$KEYS/metadata-version"
  previous="$(cat "$counter" 2>/dev/null || echo 0)"
  for existing in "$OUT"/metadata/*.targets.json; do
    [ -e "$existing" ] || continue
    seen="$(basename "$existing" .targets.json)"
    [ "$seen" -gt "$previous" ] 2>/dev/null && previous="$seen"
  done
  [ -n "${YARNGO_TUF_FROM:-}" ] && previous="$YARNGO_TUF_FROM"

  # Keys restored from a backup that did not include the counter, with no
  # output to read either. Publishing as version 1 would be read by every
  # application that has already fetched a higher one as somebody replaying an
  # old repository — they would refuse it and quietly keep what they had.
  if [ "$previous" -eq 0 ] && [ -s "$KEYS/root.json" ] && [ ! -f "$counter" ]; then
    cat >&2 <<LOST
Nothing here records which metadata version was last published, and there is no
built output to read it from.

If you have published before, find the number — it is the highest N in the
N.targets.json files currently being served — and say so:

    YARNGO_TUF_FROM=<N> scripts/tuf-repo.sh publish $version

If this is genuinely the first publication from these keys, say that instead:

    YARNGO_TUF_FROM=0 scripts/tuf-repo.sh publish $version
LOST
    exit 1
  fi
  n=$((previous + 1))

  building="$OUT.building"
  rm -rf "$building"
  mkdir -p "$(dirname "$OUT")"
  tuftool create \
    --root "$KEYS/root.json" \
    -k "$KEYS/targets.pem" -k "$KEYS/snapshot.pem" -k "$KEYS/timestamp.pem" \
    --add-targets "$staged" \
    --targets-expires "$ROLE_EXPIRY"      --targets-version   "$n" \
    --snapshot-expires "$ROLE_EXPIRY"     --snapshot-version  "$n" \
    --timestamp-expires "$TIMESTAMP_EXPIRY" --timestamp-version "$n" \
    --outdir "$building"

  # tuftool links a target back to where it read it, and where it read it is a
  # temporary directory. Take the bytes.
  find "$building/targets" -type l | while read -r link; do
    cp -L "$link" "$link.real" && mv -f "$link.real" "$link"
  done
  cp "$KEYS/root.json" "$building/root.json"

  # Built beside and swapped, so a failed publish leaves what was working.
  rm -rf "$OUT.previous"
  [ -d "$OUT" ] && mv "$OUT" "$OUT.previous"
  mv "$building" "$OUT"
  echo "$n" > "$counter"

  cat <<DONE

Published $version as metadata version $n, into $OUT

To make it real, serve those files at:

  $SERVED_AT

which for a GitHub repository means copying them in and pushing:

  cp -R $OUT/. <your-artifacts-checkout>/tuf/
  cd <your-artifacts-checkout> && git add tuf && git commit -m "Publish runtime $version" && git push

Until that lands, applications keep installing the recipe inside them.

This publication is believed until $(python3 -c "
import datetime
print((datetime.date.today() + datetime.timedelta(days=90)).isoformat())"). Re-publishing before
then is what keeps it believed; expired metadata is refused, which is the point
of it. "scripts/tuf-repo.sh status" shows the date.
DONE
}


die() { echo "$*" >&2; exit 1; }

case "${1:-}" in
init)
  [ -e "$KEYS/root.json" ] && die \
"Signing keys already exist at $KEYS.

Creating a second set would orphan every copy of the application already
shipped with the first — they trust these keys and no others. If you meant to
rotate them, that is a different and more careful job than this command."

  # A key inside the working tree is a key one 'git add .' from being public.
  case "$(cd "$(dirname "$KEYS")" 2>/dev/null && pwd || echo "$KEYS")" in
    "$PWD"|"$PWD"/*) die "Refusing to write signing keys inside the repository ($KEYS)." ;;
  esac

  mkdir -p "$KEYS"
  chmod 700 "$KEYS"

  # Root is held by three keys needing two to sign: one lost is recoverable,
  # one stolen is not enough. Each other role holds one key of its own, so the
  # timestamp key — the one that gets re-signed most often and is therefore
  # most exposed — can say a snapshot is current and cannot say which targets
  # are ours.
  tuftool root init "$KEYS/root.json"
  tuftool root expire "$KEYS/root.json" "$ROOT_EXPIRY"
  tuftool root set-threshold "$KEYS/root.json" root 2
  for n in 1 2 3; do
    tuftool root gen-rsa-key "$KEYS/root.json" "$KEYS/root-$n.pem" --role root
  done
  for role in targets snapshot timestamp; do
    tuftool root set-threshold "$KEYS/root.json" "$role" 1
    tuftool root gen-rsa-key "$KEYS/root.json" "$KEYS/$role.pem" --role "$role"
  done
  tuftool root sign "$KEYS/root.json" -k "$KEYS/root-1.pem" -k "$KEYS/root-2.pem"
  chmod 600 "$KEYS"/*.pem

  # Nothing has been published from these keys, and recording that is what
  # separates "the first publish" from "the counter went missing" — which are
  # the same absence and need opposite treatment.
  echo 0 > "$KEYS/metadata-version"

  mkdir -p "$(dirname "$ANCHOR")"
  cp "$KEYS/root.json" "$ANCHOR"

  cat <<DONE

Done. Two things exist now.

  $KEYS
      The private keys. Back this directory up somewhere you would keep a
      password. If you lose it you cannot publish to anyone who already
      installed the app; if someone else gets it they can publish to everyone
      who did.

  $ANCHOR
      The public half. Commit it:

          git add $ANCHOR && git commit -m "Ship the runtime trust anchor"

      Builds made after that can install published runtimes. Builds made
      before it cannot, ever — so anyone already running one needs a new
      version of the application, not a new runtime.

Next: scripts/tuf-repo.sh publish <version>
DONE
  ;;

refresh)
  [ -f "$KEYS/root.json" ] || die "No signing keys at $KEYS. Run: scripts/tuf-repo.sh init"
  [ -d "$OUT/targets" ] || die "Nothing published at $OUT to refresh. Run: scripts/tuf-repo.sh publish <version>"

  # The very targets being served, under their own names again. Staging from
  # the checkout instead would quietly publish whatever the working tree
  # holds, and a freshness renewal must not change what is vouched for.
  staged="$(mktemp -d)"
  trap 'rm -rf "$staged"' EXIT
  for hashed in "$OUT"/targets/*; do
    cp "$hashed" "$staged/$(basename "$hashed" | sed 's/^[0-9a-f]\{64\}\.//')"
  done
  version="$(sed -n 's/.*"version": "\([^"]*\)".*/\1/p' "$staged/catalogue.json" | head -1)"
  [ -n "$version" ] || die "The published catalogue at $OUT names no version; refusing to guess."
  sign_and_swap "$version" "$staged"
  ;;

publish)
  version="${2:-}"
  [ -n "$version" ] || die "Which version? e.g. scripts/tuf-repo.sh publish $(date +%Y.%m.%d).1"
  [ -f "$KEYS/root.json" ] || die "No signing keys at $KEYS. Run: scripts/tuf-repo.sh init"

  staged="$(mktemp -d)"
  trap 'rm -rf "$staged"' EXIT

  # The recipe as it stands in this checkout, named for the version it is being
  # published as. Nothing is invented here: what ships inside the application
  # and what is published are the same two files.
  cp packaging/packs/mlx/uv.lock        "$staged/mlx-$version.uv.lock"
  cp packaging/packs/mlx/pyproject.toml "$staged/mlx-$version.pyproject.toml"

  # The floor is this application's own version: a build older than the one
  # that produced this recipe should pass the release over rather than install
  # something it cannot drive.
  floor="$(sed -n 's/^version = "\(.*\)"$/\1/p' Cargo.toml | head -1)"
  cat > "$staged/catalogue.json" <<JSON
{
  "schema": 1,
  "runtimes": {
    "mlx": [
      { "version": "$version",
        "min_app_version": "$floor",
        "engine_api": 1,
        "lock": "mlx-$version.uv.lock",
        "pyproject": "mlx-$version.pyproject.toml" }
    ]
  }
}
JSON

  sign_and_swap "$version" "$staged"
  ;;

status)
  # A checklist with one next action, rather than a list of things that are
  # absent. Two of the three are absent *because* the first is, and reporting
  # them as separate problems reads like three failures when it is one step.
  have_keys=false;   [ -f "$KEYS/root.json" ] && have_keys=true
  have_anchor=false; [ -f "$ANCHOR" ]         && have_anchor=true
  have_built=false;  [ -d "$OUT" ]            && have_built=true

  mark() { if [ "$1" = true ]; then printf '  done  '; else printf '  todo  '; fi; }

  if $have_keys && $have_anchor; then
    echo "Runtime publishing: set up."
  elif $have_keys; then
    echo "Runtime publishing: keys exist, anchor not committed."
  else
    echo "Runtime publishing: not set up. Nothing is broken — this is where"
    echo "every checkout starts, and the application installs the runtime"
    echo "recipe inside it until the three steps below are done."
  fi
  echo

  mark $have_keys;   echo "signing keys    $KEYS"
  mark $have_anchor; echo "trust anchor    $ANCHOR"
  mark $have_built;  printf 'published       %s' "$OUT"
  if $have_built; then
    printf ' (metadata version %s)' "$(cat "$KEYS/metadata-version" 2>/dev/null || echo '?')"
  fi
  echo
  if $have_built && [ -f "$OUT/metadata/timestamp.json" ]; then
    expires="$(python3 -c "
import json,sys
print(json.load(open('$OUT/metadata/timestamp.json'))['signed']['expires'])" 2>/dev/null || echo '')"
    [ -n "$expires" ] && echo "                believed until $expires"
  fi
  echo

  if ! $have_keys; then
    echo "Next:  scripts/tuf-repo.sh init"
  elif ! $have_anchor; then
    echo "Next:  cp $KEYS/root.json $ANCHOR && git add $ANCHOR"
  elif ! $have_built; then
    echo "Next:  scripts/tuf-repo.sh publish $(date +%Y.%-m.%-d).1"
  else
    echo "Next:  serve $OUT at $SERVED_AT"
    echo "       and re-publish before the date above, which is what keeps it believed."
  fi
  ;;

*)
  sed -n '2,22p' "$0" | sed 's/^# \{0,1\}//' >&2
  exit 1
  ;;
esac
