#!/usr/bin/env bash
# Publish the catalogue and runtime recipes to the artifacts repository.
#
# What ships here is kilobytes of *recipe*, never payload: the model catalogue
# names Hugging Face repositories at pinned revisions, and the runtime packs are
# a pyproject and a uv.lock. Every heavy byte still comes from the place that
# already serves it — weights from Hugging Face, CPython from
# python-build-standalone, wheels from PyPI. If that ever changes, the payload
# belongs on object storage with free egress, not on a release page.
#
# Two tags per publication:
#
#   catalog-<date>   immutable, holds catalog.json and the pack recipes
#   latest           a moving pointer holding only manifest.json
#
# The app only ever asks for `latest/manifest.json`; everything that file names
# is pinned by tag and checked by digest. That way the URL compiled into a
# shipped binary never has to change, and nothing it fetches is mutable.
#
#   scripts/publish-artifacts.sh            # dry run: build and show, publish nothing
#   scripts/publish-artifacts.sh --publish  # create the release
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/.."

REPO="${YARNGO_ARTIFACTS_REPO:-getlatentic/yarngo-artifacts}"
TAG="catalog-$(date -u +%Y%m%d-%H%M%S)"
PUBLISH="${1:-}"

# The oldest application this publication is safe for. Raise it when an entry
# starts using a field older sidecars cannot honour; older apps then keep their
# bundled catalogue instead of misreading this one.
# While the application is pre-release, the floor has to be a pre-release too:
# SemVer puts 0.1.0-alpha.1 *below* 0.1.0, so a floor of 0.1.0 would refuse
# every alpha build the runtime updates are meant to serve. Raise this when
# there is a published recipe an older build genuinely cannot drive.
MIN_APP_VERSION="0.0.0"

STAGE="$(mktemp -d)"
trap 'rm -rf "$STAGE"' EXIT

cp packaging/catalog.json "$STAGE/catalog.json"
for pack in mlx torch; do
  cp "packaging/packs/$pack/uv.lock" "$STAGE/$pack-uv.lock"
  cp "packaging/packs/$pack/pyproject.toml" "$STAGE/$pack-pyproject.toml"
done

digest() { shasum -a 256 "$1" | awk '{print $1}'; }
base="https://github.com/$REPO/releases/download/$TAG"

# The sidecar api the catalogue declares, read from the catalogue rather than
# repeated here, so the two cannot drift.
api="$(python3 -c "import json;print(json.load(open('packaging/catalog.json'))['sidecar_api'])")"

cat > "$STAGE/manifest.json" <<JSON
{
  "schema": 1,
  "min_app_version": "$MIN_APP_VERSION",
  "sidecar_api": $api,
  "tag": "$TAG",
  "catalog": {
    "url": "$base/catalog.json",
    "sha256": "$(digest "$STAGE/catalog.json")"
  },
  "runtimes": {
    "mlx": {
      "lock_url": "$base/mlx-uv.lock",
      "lock_sha256": "$(digest "$STAGE/mlx-uv.lock")",
      "pyproject_url": "$base/mlx-pyproject.toml",
      "pyproject_sha256": "$(digest "$STAGE/mlx-pyproject.toml")"
    },
    "torch": {
      "lock_url": "$base/torch-uv.lock",
      "lock_sha256": "$(digest "$STAGE/torch-uv.lock")",
      "pyproject_url": "$base/torch-pyproject.toml",
      "pyproject_sha256": "$(digest "$STAGE/torch-pyproject.toml")"
    }
  }
}
JSON

python3 -c "import json,sys; json.load(open('$STAGE/manifest.json'))" \
  || { echo "generated manifest is not valid json" >&2; exit 1; }

echo "tag:  $TAG"
echo "repo: $REPO"
ls -la "$STAGE" | awk 'NR>3 {printf "  %8s  %s\n", $5, $9}'
echo
cat "$STAGE/manifest.json"

if [[ "$PUBLISH" != "--publish" ]]; then
  echo
  echo "dry run — nothing published. Re-run with --publish."
  exit 0
fi

gh release create "$TAG" --repo "$REPO" \
  --title "Catalogue $TAG" \
  --notes "Model catalogue and runtime recipes. Fetched by yarngo studio; every asset is digest-checked against the manifest on \`latest\`." \
  "$STAGE/catalog.json" \
  "$STAGE/mlx-uv.lock" "$STAGE/mlx-pyproject.toml" \
  "$STAGE/torch-uv.lock" "$STAGE/torch-pyproject.toml"

# `latest` is deleted and recreated rather than edited: a release cannot have an
# asset replaced in place, and a half-updated pointer is worse than a missing
# one for the few seconds this takes.
gh release delete latest --repo "$REPO" --yes --cleanup-tag 2>/dev/null || true
gh release create latest --repo "$REPO" \
  --title "Latest manifest" \
  --notes "Points at the current catalogue release. This is the only URL the app knows; everything it names is pinned and digest-checked." \
  "$STAGE/manifest.json"

echo "published $TAG, and latest now points at it"
