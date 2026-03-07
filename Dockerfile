FROM rust:1.93-bookworm AS chef

WORKDIR /app

RUN cargo install cargo-chef --locked

FROM chef AS dev

RUN cargo install cargo-watch --locked

FROM chef AS planner

COPY Cargo.toml Cargo.lock ./
COPY apps/wattetheria-cli/Cargo.toml apps/wattetheria-cli/Cargo.toml
COPY apps/wattetheria-kernel/Cargo.toml apps/wattetheria-kernel/Cargo.toml
COPY apps/wattetheria-observatory/Cargo.toml apps/wattetheria-observatory/Cargo.toml
COPY crates/conformance/Cargo.toml crates/conformance/Cargo.toml
COPY crates/control-plane/Cargo.toml crates/control-plane/Cargo.toml
COPY crates/kernel-core/Cargo.toml crates/kernel-core/Cargo.toml
COPY crates/observatory-core/Cargo.toml crates/observatory-core/Cargo.toml
COPY crates/p2p-runtime/Cargo.toml crates/p2p-runtime/Cargo.toml

RUN mkdir -p \
    apps/wattetheria-cli/src \
    apps/wattetheria-kernel/src \
    apps/wattetheria-observatory/src \
    crates/conformance/src \
    crates/control-plane/src \
    crates/kernel-core/src \
    crates/observatory-core/src \
    crates/p2p-runtime/src \
    && printf "fn main() {}\n" > apps/wattetheria-cli/src/main.rs \
    && printf "fn main() {}\n" > apps/wattetheria-kernel/src/main.rs \
    && printf "fn main() {}\n" > apps/wattetheria-observatory/src/main.rs \
    && printf "pub fn _planner_stub() {}\n" > crates/conformance/src/lib.rs \
    && printf "pub fn _planner_stub() {}\n" > crates/control-plane/src/lib.rs \
    && printf "pub fn _planner_stub() {}\n" > crates/kernel-core/src/lib.rs \
    && printf "pub fn _planner_stub() {}\n" > crates/observatory-core/src/lib.rs \
    && printf "pub fn _planner_stub() {}\n" > crates/p2p-runtime/src/lib.rs

RUN cargo chef prepare --recipe-path recipe.json

FROM chef AS cacher

COPY --from=planner /app/recipe.json /app/recipe.json
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/usr/local/cargo/git \
    cargo chef cook --release --recipe-path recipe.json \
    -p wattetheria-kernel -p wattetheria-observatory

FROM chef AS builder

COPY . .
COPY --from=cacher /app/target /app/target

RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/usr/local/cargo/git \
    cargo build --release -p wattetheria-kernel -p wattetheria-observatory

FROM debian:bookworm-slim

RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates curl \
    && rm -rf /var/lib/apt/lists/*

RUN useradd --create-home --uid 10001 wattetheria

WORKDIR /app

COPY --from=builder /app/target/release/wattetheria-kernel /app/target/release/wattetheria-kernel
COPY --from=builder /app/target/release/wattetheria-observatory /app/target/release/wattetheria-observatory
COPY --from=builder /app/scripts/docker-kernel-entrypoint.sh /app/scripts/docker-kernel-entrypoint.sh
COPY --from=builder /app/scripts/docker-observatory-entrypoint.sh /app/scripts/docker-observatory-entrypoint.sh

RUN mkdir -p /var/lib/wattetheria \
    && chmod +x /app/scripts/docker-kernel-entrypoint.sh /app/scripts/docker-observatory-entrypoint.sh \
    && chown -R wattetheria:wattetheria /var/lib/wattetheria /app

USER wattetheria

EXPOSE 7777
EXPOSE 8787

ENTRYPOINT ["/app/scripts/docker-kernel-entrypoint.sh"]
