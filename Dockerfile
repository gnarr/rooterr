# syntax=docker/dockerfile:1

FROM rust:1.95-bookworm AS builder

WORKDIR /app
COPY Cargo.toml Cargo.lock* ./
COPY src ./src
RUN --mount=type=cache,target=/app/target \
    --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/usr/local/cargo/git/db \
    cargo build --release && \
    mkdir -p /app/build && \
    cp /app/target/release/rooterr /app/build/rooterr

FROM debian:bookworm-slim

RUN useradd --system --create-home --uid 10001 rooterr
WORKDIR /app
COPY --from=builder /app/build/rooterr /usr/local/bin/rooterr
COPY rooterr.toml.example /app/rooterr.toml.example
RUN mkdir -p /app/data && chown -R rooterr:rooterr /app/data

USER rooterr
EXPOSE 9898
ENV ROOTERR_CONFIG=/config/rooterr.toml
CMD ["rooterr"]
