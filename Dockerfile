# Single image: the SvelteKit frontend is built and baked in alongside the axum binary,
# which serves it (ServeDir + SPA fallback) on the same origin as /api/v1. One image, one
# Fly app — the same pattern as UMS and optimizer.

# ── Web stage: build the static SvelteKit SPA ────────────────────────────────
# Separate base from the Rust stages so the two are cache-independent: touching Rust
# code doesn't rebuild the frontend, and vice versa.
FROM node:20-slim AS web
WORKDIR /web
# Dependencies first, so app-code changes don't re-run npm ci.
COPY web/package.json web/package-lock.json ./
RUN npm ci
COPY web/src ./src
COPY web/static ./static
COPY web/svelte.config.js web/vite.config.ts web/tsconfig.json web/tailwind.config.js web/postcss.config.js ./
RUN npm run build

# ── Rust stages ──────────────────────────────────────────────────────────────
# 1.96 matches the local toolchain. The old 1.82 pin no longer builds: a transitive dep now
# requires the `edition2024` cargo feature, which 1.82 doesn't stabilize, so `cargo chef cook`
# panics before compiling anything.
#
# trixie, NOT bookworm: fastembed pulls in the ONNX Runtime (`ort`/`ort_sys`), whose prebuilt
# binary is compiled against glibc >= 2.38 (it references `__isoc23_strtol`, `__cxa_call_terminate`).
# Bookworm ships glibc 2.36, so the final link dies in ~100 undefined references. Trixie has 2.41.
FROM lukemathwalker/cargo-chef:latest-rust-1.96-slim-trixie AS chef
WORKDIR /app

# build-essential + cmake: the dependency tree compiles/links C/C++ (the ONNX runtime,
# tokenizers/onig_sys, `ring`). Without a C++ toolchain the link fails on `cannot find -lstdc++`.
RUN apt-get update && apt-get install -y \
  pkg-config libssl-dev build-essential cmake \
  && rm -rf /var/lib/apt/lists/*

# ── Plan stage ──────────────────────────────────────────────────────────────
# Copy ONLY the manifests, never the sources. The dependency graph is fully determined by
# Cargo.toml + Cargo.lock, and Docker hashes this stage's inputs to decide whether the
# `cargo chef cook` layer below is still valid. A `COPY . .` here would mean any edit to any
# .rs file changed the recipe's inputs and re-cooked the whole dependency tree (ONNX,
# tokenizers, image codecs — minutes each time), which defeats the entire point of chef.
# `prepare` still needs the [[bin]] target's path to exist, so stub it.
FROM chef AS planner
COPY Cargo.toml Cargo.lock ./
RUN mkdir -p src && echo "fn main() {}" > src/main.rs
RUN cargo chef prepare --recipe-path recipe.json

# ── Build stage ──────────────────────────────────────────────────────────────
FROM chef AS builder
COPY --from=planner /app/recipe.json recipe.json
RUN cargo chef cook --release --recipe-path recipe.json

COPY . .
# Compile against the checked-in `.sqlx` query cache — there's no database reachable
# at image-build time, so the `query!` macros must not try to hit one.
ENV SQLX_OFFLINE=true
RUN cargo build --release --bin flaborful-api

# ── Runtime stage ─────────────────────────────────────────────────────────────
# trixie must match the builder: the binary is linked against that image's glibc (2.41), so a
# bookworm runtime (2.36) would fail to load it.
FROM debian:trixie-slim AS runtime
WORKDIR /app

# libstdc++6: the ONNX runtime links against it at run time, not just at build time.
RUN apt-get update && apt-get install -y \
  ca-certificates openssl tini libstdc++6 \
  && rm -rf /var/lib/apt/lists/*

COPY --from=builder /app/target/release/flaborful-api /usr/local/bin/flaborful-api
# The static frontend the binary serves via ServeDir + SPA fallback (see WEB_DIR).
COPY --from=web /web/build /srv/web

ENV WEB_DIR=/srv/web

EXPOSE 8080

ENTRYPOINT ["/usr/bin/tini", "--"]
CMD ["/usr/local/bin/flaborful-api"]
