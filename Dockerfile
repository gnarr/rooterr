FROM rust:1.95-bookworm AS builder

WORKDIR /app
COPY Cargo.toml Cargo.lock* ./
COPY src ./src
RUN cargo build --release

FROM debian:bookworm-slim

RUN useradd --system --create-home --uid 10001 rooterr
WORKDIR /app
COPY --from=builder /app/target/release/rooterr /usr/local/bin/rooterr
COPY rooterr.toml.example /app/rooterr.toml.example
RUN mkdir -p /app/data && chown -R rooterr:rooterr /app/data

USER rooterr
EXPOSE 9898
ENV ROOTERR_CONFIG=/config/rooterr.toml
CMD ["rooterr"]
