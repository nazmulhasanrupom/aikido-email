# syntax=docker/dockerfile:1.7
# One image: the Rust server with the web app inside it.

# ---- 1. the web app ----
FROM node:22-bookworm-slim AS ui
RUN npm install -g pnpm@10
WORKDIR /ui
COPY ui/package.json ui/pnpm-lock.yaml ui/pnpm-workspace.yaml ./
RUN pnpm install --frozen-lockfile
COPY ui/ ./
RUN pnpm build

# ---- 2. the server ----
FROM rust:1-bookworm AS server
WORKDIR /app
COPY Cargo.toml Cargo.lock build.rs ./
COPY src ./src
COPY migrations ./migrations
COPY docs ./docs
COPY --from=ui /ui/dist ./ui/dist
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/app/target \
    cargo build --release && cp target/release/aikido_email /aikido_email

# ---- 3. the small image that runs ----
FROM debian:bookworm-slim
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates curl \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --system --uid 10001 --home-dir /data --shell /usr/sbin/nologin aikido \
    && mkdir /data && chown aikido:aikido /data
COPY --from=server /aikido_email /usr/local/bin/aikido_email
USER aikido
ENV AIKIDO_DATA_DIR=/data \
    AIKIDO_BIND_ADDR=0.0.0.0:8080 \
    RUST_LOG=info
VOLUME /data
EXPOSE 8080
HEALTHCHECK --interval=30s --timeout=5s --start-period=40s --retries=3 \
    CMD curl -fsS http://127.0.0.1:8080/health || exit 1
ENTRYPOINT ["aikido_email"]
CMD ["serve"]
