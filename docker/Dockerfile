# syntax=docker/dockerfile:1.7
FROM rust:1.94-bookworm AS builder
WORKDIR /app

COPY Cargo.toml Cargo.lock rust-toolchain.toml ./
COPY src ./src
COPY migrations ./migrations
COPY skills ./skills
COPY IDENTITY.md ./IDENTITY.md
COPY docs ./docs
COPY templates ./templates
COPY static ./static

RUN cargo build --release

FROM debian:bookworm-slim AS runtime
RUN useradd -m -u 10001 aimicroagents
WORKDIR /app

COPY --from=builder /app/target/release/ai-microagents /usr/local/bin/ai-microagents
COPY --from=builder /app/migrations ./migrations
COPY --from=builder /app/skills ./skills
COPY --from=builder /app/IDENTITY.md ./IDENTITY.md
COPY --from=builder /app/docs ./docs
COPY --from=builder /app/templates ./templates
COPY --from=builder /app/static ./static

RUN mkdir -p /app/data && chown -R aimicroagents:aimicroagents /app
USER aimicroagents

ENV FERRUM_DATABASE_PATH=/app/data/ai-microagents.db
ENV FERRUM_IDENTITY_PATH=/app/IDENTITY.md
ENV FERRUM_SKILLS_DIR=/app/skills
ENV FERRUM_BIND_ADDR=0.0.0.0:8080

EXPOSE 8080
ENTRYPOINT ["/usr/local/bin/ai-microagents", "run"]
