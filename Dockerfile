# syntax=docker/dockerfile:1

# ── Stage 1: Build ───────────────────────────────────────────────────────────
FROM rust:bookworm AS builder

RUN apt-get update && apt-get install -y --no-install-recommends \
    cmake \
    pkg-config \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /app

# Cache dependencies by building them first
COPY Cargo.toml Cargo.lock ./
RUN mkdir -p src && echo 'fn main() {}' > src/main.rs \
    && mkdir -p src/bin && echo 'fn main() {}' > src/bin/git-over.rs \
    && cargo build --release --features vendored \
    && rm -rf src

# Build the actual application
COPY . .
RUN touch src/main.rs src/bin/git-over.rs \
    && cargo build --release --features vendored

# ── Stage 2: Runtime ─────────────────────────────────────────────────────────
FROM gcr.io/distroless/cc-debian12

COPY --from=builder /app/target/release/over /usr/local/bin/over
COPY --from=builder /app/target/release/git-over /usr/local/bin/git-over

ENTRYPOINT ["over"]
