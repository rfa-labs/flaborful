#!/usr/bin/env bash
set -eu

PGPASS=$(fly ssh console -a flaborful-db -C "sh -c 'echo \$OPERATOR_PASSWORD'")
PORT=5434

fly proxy $PORT:5432 -a flaborful-db &
trap "pkill -P $$" EXIT INT

sleep 3

export DATABASE_URL=postgres://postgres:$PGPASS@localhost:$PORT/flaborful
sqlx migrate run --source migrations
