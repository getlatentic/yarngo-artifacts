#!/usr/bin/env bash
# Creates the signed repository the application fetches runtimes from, and the
# root role that ships inside it.
#
#   scripts/tuf-repo.sh init  <keys-dir> <out-dir>
#   scripts/tuf-repo.sh build <keys-dir> <out-dir> <targets-dir>
#
# `init` produces the keys and a signed root.json. Run it once. The root role
# is held by three keys with a threshold of two, so one key lost is recoverable
# and one key stolen is not enough; keep the three apart — a hardware token, a
# KMS, an offline machine — and never all three anywhere that builds the
# application. The other roles each hold one key: targets and snapshot can live
# in a KMS; timestamp is the one an automated re-signer holds, and it is the
# least powerful on purpose — it can say a snapshot is current and cannot say
# which targets are ours. tuftool signs from AWS KMS and SSM; `--key` takes
# those as URLs.
#
# `build` publishes what is in <targets-dir> as version N+1. Nothing about a
# target is described here: the catalogue names them, and the catalogue is a
# target itself. It builds a whole repository beside the old one and swaps, so
# republishing always repairs a served directory somebody has edited, and a
# failed publish leaves what was working exactly where it was.
#
# Roles hold separate keys on purpose. Timestamp is re-signed often and is the
# one most exposed; it can say a snapshot is current and cannot say which
# targets are ours.
set -euo pipefail

command -v tuftool >/dev/null || { echo "tuftool not installed: cargo install tuftool --locked" >&2; exit 1; }

usage() { sed -n '2,20p' "$0" | sed 's/^# \{0,1\}//' >&2; exit 1; }
[ $# -ge 3 ] || usage
action=$1 keys=$2 out=$3

# Root expires furthest out because rotating it means shipping an application.
# Timestamp expires soonest because that is what freshness means: metadata that
# nobody has re-signed lately stops being believed.
root_expiry='in 52 weeks'
targets_expiry='in 26 weeks'
snapshot_expiry='in 26 weeks'
timestamp_expiry='in 7 days'

case "$action" in
  init)
    mkdir -p "$keys"
    tuftool root init "$keys/root.json"
    tuftool root expire "$keys/root.json" "$root_expiry"
    tuftool root set-threshold "$keys/root.json" root 2
    for n in 1 2 3; do
      tuftool root gen-rsa-key "$keys/root.json" "$keys/root-$n.pem" --role root
    done
    for role in targets snapshot timestamp; do
      tuftool root set-threshold "$keys/root.json" "$role" 1
      tuftool root gen-rsa-key "$keys/root.json" "$keys/$role.pem" --role "$role"
    done
    tuftool root sign "$keys/root.json" -k "$keys/root-1.pem" -k "$keys/root-2.pem"

    mkdir -p "$out"
    cp "$keys/root.json" "$out/root.json"
    echo "signed root role: $out/root.json"
    echo "ship it as packaging/tuf/root.json — the application trusts nothing without it"
    ;;

  build)
    [ $# -eq 4 ] || usage
    targets=$4
    [ -f "$targets/catalogue.json" ] || {
      echo "$targets/catalogue.json is missing; without it there is nothing to install" >&2
      exit 1
    }

    # One past whatever is published, so a repeat publish is not a rollback.
    version=$(( $(ls "$out/metadata" 2>/dev/null | sed -n 's/^\([0-9]*\)\.targets\.json$/\1/p' | sort -n | tail -1 || echo 0) + 1 ))

    staging="$out.publishing"
    rm -rf "$staging"
    tuftool create \
      --root "$keys/root.json" \
      -k "$keys/targets.pem" -k "$keys/snapshot.pem" -k "$keys/timestamp.pem" \
      --add-targets "$targets" \
      --targets-expires "$targets_expiry" --targets-version "$version" \
      --snapshot-expires "$snapshot_expiry" --snapshot-version "$version" \
      --timestamp-expires "$timestamp_expiry" --timestamp-version "$version" \
      --outdir "$staging"

    # tuftool links a target back to where it read it, which is no use to a
    # server. Take the bytes.
    find "$staging/targets" -type l | while read -r link; do
      cp -L "$link" "$link.real" && mv -f "$link.real" "$link"
    done

    cp "$keys/root.json" "$staging/root.json"
    rm -rf "$out.previous"
    [ -d "$out" ] && mv "$out" "$out.previous"
    mv "$staging" "$out"

    echo "published version $version to $out"
    ;;

  *) usage ;;
esac
