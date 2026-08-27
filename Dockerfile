# Kaleido headless server (S1)
# Build with rust 1.86+ preferred; 1.85 works if idna/icu stack resolves.
FROM rust:1.86-bookworm AS builder
WORKDIR /app
COPY Cargo.toml Cargo.lock* ./
COPY crates ./crates
RUN cargo build --release -p kaleido-server

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates && rm -rf /var/lib/apt/lists/* \
    && useradd --system --uid 10001 --create-home kaleido
WORKDIR /app
COPY --from=builder /app/target/release/kaleido-server /usr/local/bin/kaleido-server
ENV KALEIDO_HOST=0.0.0.0
ENV KALEIDO_PORT=18766
ENV KALEIDO_DATA=/data
EXPOSE 18766
VOLUME ["/data"]
USER kaleido
ENTRYPOINT ["/usr/local/bin/kaleido-server"]
