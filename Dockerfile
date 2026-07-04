# ── Build stage ───────────────────────────────────────────────────────
FROM rust:slim-bookworm AS builder

WORKDIR /app
COPY Cargo.toml Cargo.lock ./
COPY src ./src

RUN cargo build --release

# ── Runtime stage ─────────────────────────────────────────────────────
FROM debian:bookworm-slim

RUN apt-get update && \
    apt-get install -y --no-install-recommends ca-certificates && \
    rm -rf /var/lib/apt/lists/*

WORKDIR /app

COPY --from=builder /app/target/release/bonjourdijon /usr/local/bin/bonjourdijon
COPY templates ./templates
COPY static ./static

EXPOSE 3000

ENTRYPOINT ["bonjourdijon"]
CMD ["serve"]
