#!/usr/bin/env bash
# Cross-build and package entirely on Linux with Haiku's host tools:
# artifacts/haiku/effectcraft-<version>-x86_64.hpkg. airos-ci builds the image packages the same
# way (build_effectcraft in apps/build-app-packages.sh).
set -euo pipefail
ROOT=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd)
cd "$ROOT"
. tools/haiku/env.sh
TOOLS=${HAIKU_HOST_TOOLS:-/mnt/HaikuWork/build/fw13-x86_64/objects/linux/x86_64/release/tools}
RES="$ROOT/apps/effectcraft-haiku/resources"
OUT="$ROOT/artifacts/haiku"
VERSION=$(awk '$1 == "version" { print $2; exit }' "$RES/EffectCraft.PackageInfo")
if [[ ${1:-} != --no-build ]]; then bash tools/haiku/build.sh; fi
mkdir -p "$OUT" "$ROOT/target/tmp"
STAGE=$(mktemp -d "$ROOT/target/tmp/effectcraft-pkg-XXXXXX")
trap 'rm -rf -- "$STAGE"' EXIT
DOCS="$STAGE/documentation/packages/effectcraft"
mkdir -p "$STAGE/apps" "$STAGE/data/deskbar/menu/Media" "$STAGE/boot/post-install" "$DOCS"
"$HAIKU_CROSS-strip" -o "$STAGE/apps/EffectCraft" "$CARGO_TARGET_DIR/x86_64-unknown-haiku/release/EffectCraft"
(cd "$RES" && "$TOOLS/rc/rc" -o "$STAGE/EffectCraft.rsrc" EffectCraft.rdef)
"$TOOLS/xres" -o "$STAGE/apps/EffectCraft" "$STAGE/EffectCraft.rsrc"
"$TOOLS/resattr/resattr" -O -o "$STAGE/apps/EffectCraft" "$STAGE/EffectCraft.rsrc"
rm "$STAGE/EffectCraft.rsrc"
cp "$RES/EffectCraft.PackageInfo" "$STAGE/.PackageInfo"
cp "$RES/effectcraft-post-install.sh" "$STAGE/boot/post-install/effectcraft.sh"
chmod 755 "$STAGE/boot/post-install/effectcraft.sh" "$STAGE/apps/EffectCraft"
cp README.md LICENSE-MIT LICENSE-APACHE NOTICE ATTRIBUTION.md docs/haiku.md "$DOCS/"
# The licence texts of the assets and vendored crates the app embeds.
while IFS= read -r license; do
	mkdir -p "$DOCS/licenses/$(dirname -- "$license")"
	cp "$license" "$DOCS/licenses/$license"
done < <(find assets crates vendor apps/effectcraft-haiku tools/haiku -type f -regextype posix-extended -regex '(.*/)?(LICENSE|OFL|COPYING)[^/]*' | sort)
# egui's default fonts come from the storytold/egui commit patched in Cargo.toml (a git checkout).
mkdir -p "$DOCS/licenses/epaint_default_fonts"
EGUI_REV=$(sed -n 's/^epaint_default_fonts = .*rev = "\([0-9a-f]*\)".*/\1/p' Cargo.toml)
cp "$CARGO_HOME"/git/checkouts/egui-*/"${EGUI_REV:0:7}"/crates/epaint_default_fonts/fonts/*.txt "$DOCS/licenses/epaint_default_fonts/"
ln -s ../../../../apps/EffectCraft "$STAGE/data/deskbar/menu/Media/EffectCraft"
"$TOOLS/package/package" create -C "$STAGE" "$OUT/effectcraft-$VERSION-x86_64.hpkg"
echo "$OUT/effectcraft-$VERSION-x86_64.hpkg"
