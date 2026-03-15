# syntax=docker/dockerfile:1.7
FROM rust:1.94-bookworm AS builder
WORKDIR /app

COPY Cargo.toml Cargo.lock rust-toolchain.toml ./
COPY src ./src
COPY migrations ./migrations
COPY skills ./skills
COPY IDENTITY.md ./IDENTITY.md

RUN cargo build --release

FROM debian:bookworm-slim AS runtime
RUN useradd -m -u 10001 ferrum
WORKDIR /app

COPY --from=builder /app/target/release/ferrum /usr/local/bin/ferrum
COPY --from=builder /app/migrations ./migrations
COPY --from=builder /app/skills ./skills
COPY --from=builder /app/IDENTITY.md ./IDENTITY.md

RUN mkdir -p /app/data && chown -R ferrum:ferrum /app
USER ferrum

ENV FERRUM_DATABASE_PATH=/app/data/ferrum.db
ENV FERRUM_IDENTITY_PATH=/app/IDENTITY.md
ENV FERRUM_SKILLS_DIR=/app/skills
ENV FERRUM_BIND_ADDR=0.0.0.0:8080

EXPOSE 8080
ENTRYPOINT ["/usr/local/bin/ferrum", "run"]
