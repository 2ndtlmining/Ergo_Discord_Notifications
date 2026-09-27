# syntax=docker/dockerfile:1.7
#
# Everything compiles inside Docker; no local Rust toolchain is needed.
#
#   docker build -t ergo-monitor .                                   # runtime image (default)
#   docker build --target test .                                     # fmt check + clippy + tests
#   docker build --target fmt  -o type=local,dest=. .                # write formatted sources back
#   docker build --target lock -o type=local,dest=.lock-out .        # regenerate Cargo.lock
#
# See scripts/*.ps1 for wrappers.

# ---------- build ----------
FROM rust:1-slim-bookworm AS build
WORKDIR /app

RUN rustup component add rustfmt clippy

COPY Cargo.toml Cargo.lock* ./
COPY src ./src

ARG GIT_SHA=dev
ARG BUILT_AT=unknown
ENV ERGO_MONITOR_COMMIT=$GIT_SHA
ENV ERGO_MONITOR_BUILT_AT=$BUILT_AT

# --locked: a stale or missing Cargo.lock must fail the build rather than be
# silently regenerated. Use the `lock` target to refresh it deliberately.
RUN --mount=type=cache,target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,target=/app/target,sharing=locked \
    cargo build --release --locked \
 && mkdir -p /out \
 && cp target/release/ergo-monitor /out/ergo-monitor

# ---------- lock ----------
# The only stage allowed to resolve dependencies freely; exports just Cargo.lock.
FROM rust:1-slim-bookworm AS lock-run
WORKDIR /app
COPY Cargo.toml Cargo.lock* ./
COPY src ./src
RUN --mount=type=cache,target=/usr/local/cargo/registry,sharing=locked \
    cargo generate-lockfile

FROM scratch AS lock
COPY --from=lock-run /app/Cargo.lock /Cargo.lock

# ---------- fmt ----------
FROM build AS fmt-run
RUN cargo fmt --all

FROM scratch AS fmt
COPY --from=fmt-run /app/src /src

# ---------- test ----------
FROM build AS test
RUN --mount=type=cache,target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,target=/app/target,sharing=locked \
    cargo fmt --all -- --check \
 && cargo clippy --all-targets --locked -- -D warnings \
 && cargo test --locked

# ---------- runtime ----------
FROM debian:bookworm-slim AS runtime

RUN apt-get update \
 && apt-get install -y --no-install-recommends ca-certificates \
 && rm -rf /var/lib/apt/lists/* \
 && useradd --create-home --shell /usr/sbin/nologin ergo

COPY --from=build /out/ergo-monitor /usr/local/bin/ergo-monitor

USER ergo
WORKDIR /home/ergo

ENV HTTP_PORT=7777
EXPOSE 7777

HEALTHCHECK --interval=30s --timeout=5s --start-period=10s --retries=3 \
  CMD ["/usr/local/bin/ergo-monitor", "healthcheck"]

CMD ["/usr/local/bin/ergo-monitor"]
