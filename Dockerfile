# syntax=docker/dockerfile:1

FROM gcr.io/distroless/cc-debian12

# Local builds use mise's output; CI selects its downloaded binary instead.
ARG BINARY=target/debug/over
COPY --chmod=755 ${BINARY} /usr/local/bin/over

ENTRYPOINT ["over"]
