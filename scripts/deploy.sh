#!/usr/bin/env bash
set -eu

echo "Running migrations..."
bash scripts/migrate.sh

echo "Deploying app..."
fly deploy --app flaborful-api
