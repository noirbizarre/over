# syntax=docker/dockerfile:1

FROM debian:bookworm-slim

# Attribute inspection and configured Git dependencies need Git even for immutable sources.
RUN apt-get update \
    && apt-get install -y --no-install-recommends git ca-certificates libgcc-s1 libssl3 \
    && rm -rf /var/lib/apt/lists/*

# Local builds use mise's output; CI selects its downloaded binary instead.
ARG BINARY=target/debug/over
COPY --chmod=755 ${BINARY} /usr/local/bin/over

ENTRYPOINT ["over"]
CMD ["--help"]
