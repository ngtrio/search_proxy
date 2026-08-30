FROM rust:1.94-bookworm AS rust
WORKDIR /app
COPY Cargo.toml Cargo.lock ./
COPY migrations migrations
COPY schemas schemas
COPY src src
RUN cargo build --release --locked

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates curl && rm -rf /var/lib/apt/lists/*
RUN useradd --system --uid 10001 gateway && mkdir /data && chown gateway:gateway /data
COPY --from=rust /app/target/release/tavily-mcp-gateway /usr/local/bin/gateway
USER gateway
ENV DATABASE_URL=sqlite:///data/gateway.db?mode=rwc GATEWAY_BIND=0.0.0.0:3000
EXPOSE 3000
ENTRYPOINT ["gateway"]
