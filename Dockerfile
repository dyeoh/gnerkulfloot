# Multi-stage build. cargo-chef caches compiled dependencies in their own layer,
# so a code-only change rebuilds in seconds instead of minutes.
FROM lukemathwalker/cargo-chef:latest-rust-1 AS chef
WORKDIR /app

FROM chef AS planner
COPY . .
RUN cargo chef prepare --recipe-path recipe.json

FROM chef AS builder
COPY --from=planner /app/recipe.json recipe.json
RUN cargo chef cook --release --recipe-path recipe.json
COPY . .
# Compile-time-checked queries read .sqlx/ instead of needing a live database.
ENV SQLX_OFFLINE=true
RUN cargo build --release --bin gnerkulfloot

# Distroless: no shell, no package manager, runs as non-root. Migrations are
# embedded in the binary, so it is the only file we need.
FROM gcr.io/distroless/cc-debian12:nonroot
COPY --from=builder /app/target/release/gnerkulfloot /usr/local/bin/gnerkulfloot
EXPOSE 8080
ENTRYPOINT ["/usr/local/bin/gnerkulfloot"]
CMD ["serve"]
