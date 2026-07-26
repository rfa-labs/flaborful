#!/usr/bin/env bash
# Build + package fbx-agent for npm.
#
# Cross-compiles a STATIC musl linux-x64 binary in a Linux container (so it runs on any Sprite
# regardless of the base image's glibc), vendors it into the npm package, and shows what would ship.
# The npm package (@rfa-labs/fbx-agent, PUBLIC) is how the Sprite installs fbx-agent at run time —
# the same pattern as @anthropic-ai/claude-code / @openai/codex.
#
# Usage:
#   ./build-npm.sh              # build + vendor + `npm pack --dry-run` (no publish)
#   ./build-npm.sh --publish    # the above, then `npm publish --access public`
#
# Requirements: Docker (running); npm (logged in, only for --publish).
set -euo pipefail

CRATE_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PKG_DIR="$CRATE_DIR/npm"
TARGET="x86_64-unknown-linux-musl"
# Pin the builder image to a Rust new enough to parse current deps (see the flaborful Dockerfile).
BUILDER_IMAGE="rust:1.96-slim-trixie"

echo "==> Cross-compiling fbx-agent ($TARGET, static) in $BUILDER_IMAGE"
# Mount ONLY the crate; the standalone crate has no workspace parent, so it builds on its own.
docker run --rm --platform linux/amd64 \
  -v "$CRATE_DIR":/crate -w /crate "$BUILDER_IMAGE" bash -c "
    set -e
    rustup target add $TARGET >/dev/null
    apt-get update -qq && apt-get install -y -qq musl-tools >/dev/null
    cargo build --release --target $TARGET
  "

BIN="$CRATE_DIR/target/$TARGET/release/fbx-agent"
[ -x "$BIN" ] || { echo "ERROR: build produced no binary at $BIN" >&2; exit 1; }

echo "==> Verifying the binary is statically linked (no glibc dependency)"
docker run --rm --platform linux/amd64 -v "$CRATE_DIR":/crate "$BUILDER_IMAGE" \
  bash -c "ldd /crate/target/$TARGET/release/fbx-agent 2>&1 | grep -q 'statically linked'" \
  && echo "    ok: statically linked" \
  || { echo "ERROR: binary is NOT static — it would fail on sprites with an older glibc" >&2; exit 1; }

echo "==> Vendoring the binary into $PKG_DIR/vendor/linux-x64/"
mkdir -p "$PKG_DIR/vendor/linux-x64"
cp "$BIN" "$PKG_DIR/vendor/linux-x64/fbx-agent"
chmod +x "$PKG_DIR/vendor/linux-x64/fbx-agent"

echo "==> Package contents (npm pack --dry-run):"
( cd "$PKG_DIR" && npm pack --dry-run )

if [ "${1:-}" = "--publish" ]; then
  echo "==> Publishing @rfa-labs/fbx-agent (public)"
  ( cd "$PKG_DIR" && npm publish --access public )
  echo "==> Published. Bump the version in npm/package.json before the next publish."
else
  echo
  echo "Done. To publish:  ./build-npm.sh --publish   (npm login first)"
fi
