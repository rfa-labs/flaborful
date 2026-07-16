#!/usr/bin/env bash
# Push the app's secrets to Fly, read straight from the local .env.
#
# Deliberately reads the file rather than taking values on the command line, so production
# secrets never land in shell history or a transcript.
#
# Skipped on purpose:
#   DATABASE_URL   — set for you by `fly postgres attach`
#   JWT_SECRET     — generated fresh here; never reuse the dev value
#   the URL/config vars (API_BASE_URL, the OAuth redirects, RUST_LOG, WEB_DIR) — these are
#                    non-secret and live in fly.toml [env]
#   any var that is empty in .env (e.g. GOOGLE_* until you set up Workspace)
set -euo pipefail

APP="${1:-flaborful-api}"
ENV_FILE="${2:-.env}"

[ -f "$ENV_FILE" ] || { echo "no $ENV_FILE"; exit 1; }

SKIP='^(DATABASE_URL|JWT_SECRET|RUST_LOG|WEB_DIR|API_BASE_URL|GITHUB_OAUTH_REDIRECT_URI|GOOGLE_OAUTH_REDIRECT_URI)$'

args=()
# A fresh 32-byte signing key. The app now refuses to boot without this (it used to fall back
# to a hardcoded "dev_secret", which would accept forged tokens for any user).
args+=("JWT_SECRET=$(openssl rand -base64 32)")

skipped=()
while IFS='=' read -r key value; do
  [[ -z "$key" || "$key" == \#* ]] && continue
  [[ -z "$value" ]] && { skipped+=("$key (empty)"); continue; }
  [[ "$key" =~ $SKIP ]] && continue
  args+=("$key=$value")
done < <(grep -v '^[[:space:]]*#' "$ENV_FILE" | grep '=')

echo "Setting ${#args[@]} secrets on $APP:"
for a in "${args[@]}"; do echo "  ${a%%=*}"; done
[ ${#skipped[@]} -gt 0 ] && { echo "Skipping:"; for s in "${skipped[@]}"; do echo "  $s"; done; }
echo

fly secrets set -a "$APP" "${args[@]}"
