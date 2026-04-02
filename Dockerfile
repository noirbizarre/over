# syntax=docker/dockerfile:1

# ── Stage 1: Chef base ──────────────────────────────────────────────────────
FROM lukemathwalker/cargo-chef:latest-rust-bookworm AS chef

RUN apt-get update && apt-get install -y --no-install-recommends \
    cmake \
    pkg-config \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /app

# ── Stage 2: Planner ────────────────────────────────────────────────────────
FROM chef AS planner

COPY Cargo.toml Cargo.lock ./
COPY src ./src
RUN cargo chef prepare --recipe-path recipe.json

# ── Stage 3: Builder ────────────────────────────────────────────────────────
FROM chef AS builder

COPY --from=planner /app/recipe.json recipe.json
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/usr/local/cargo/git \
    cargo chef cook --release --features vendored --recipe-path recipe.json

COPY . .
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/usr/local/cargo/git \
    cargo build --release --features vendored

# ── Stage 4: Runtime ────────────────────────────────────────────────────────
FROM gcr.io/distroless/cc-debian12

COPY --from=builder /app/target/release/over /usr/local/bin/over
COPY --from=builder /app/target/release/git-over /usr/local/bin/git-over

ENTRYPOINT ["over"]
