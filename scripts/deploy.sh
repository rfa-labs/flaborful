#!/usr/bin/env bash
set -eu

# Migrations are NOT run from here: `db::run_migrations` runs at boot in main.rs, so the deploy
# self-migrates against the DATABASE_URL Fly injects. Running them from a laptop first (the old
# `scripts/migrate.sh` step) meant opening a `fly proxy` to prod Postgres just to do work the app
# does anyway — and a proxy hiccup would block the deploy for no reason.
#
# scripts/migrate.sh is kept as a manual escape hatch (inspecting/repairing prod schema by hand).

fly deploy --app flaborful-api "$@"
