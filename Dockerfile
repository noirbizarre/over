# syntax=docker/dockerfile:1

# Stage the canonical binary as `over` in the build context; never rebuild it here.
FROM gcr.io/distroless/cc-debian12

COPY --chmod=755 over /usr/local/bin/over

ENTRYPOINT ["over"]
