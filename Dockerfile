FROM lukemathwalker/cargo-chef:latest-rust-1.82-slim-bookworm AS chef
WORKDIR /app

RUN apt-get update && apt-get install -y \
  pkg-config libssl-dev \
  && rm -rf /var/lib/apt/lists/*

# ── Plan stage ──────────────────────────────────────────────────────────────
FROM chef AS planner
COPY . .
RUN cargo chef prepare --recipe-path recipe.json

# ── Build stage ──────────────────────────────────────────────────────────────
FROM chef AS builder
COPY --from=planner /app/recipe.json recipe.json
RUN cargo chef cook --release --recipe-path recipe.json

COPY . .
RUN cargo build --release --bin flaborful-api

# ── Runtime stage ─────────────────────────────────────────────────────────────
FROM debian:bookworm-slim AS runtime
WORKDIR /app

RUN apt-get update && apt-get install -y \
  ca-certificates openssl tini \
  && rm -rf /var/lib/apt/lists/*

COPY --from=builder /app/target/release/flaborful-api /usr/local/bin/flaborful-api

EXPOSE 8080

ENTRYPOINT ["/usr/bin/tini", "--"]
CMD ["/usr/local/bin/flaborful-api"]
