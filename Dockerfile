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
COPY crates/node-core/Cargo.toml crates/node-core/Cargo.toml
COPY crates/observatory-core/Cargo.toml crates/observatory-core/Cargo.toml

# Replace local path dependencies with git sources for Docker builds.
# This lets users build the image without cloning watt-did / watt-wallet.
# 1) kernel-core: swap path deps to git
RUN sed -i \
    -e 's|watt-did = { path = "../../../watt-did" }|watt-did = { git = "https://github.com/wattetheria/watt-did.git" }|' \
    -e 's|watt-wallet = { path = "../../../watt-wallet" }|watt-wallet = { git = "https://github.com/wattetheria/watt-wallet.git" }|' \
    crates/kernel-core/Cargo.toml
# 2) root Cargo.toml: patch watt-wallet's internal path dep on watt-did
RUN printf '\n[patch."https://github.com/wattetheria/watt-wallet.git"]\nwatt-did = { git = "https://github.com/wattetheria/watt-did.git" }\n' \
    >> Cargo.toml

RUN mkdir -p \
    apps/wattetheria-cli/src \
    apps/wattetheria-kernel/src \
    apps/wattetheria-observatory/src \
    crates/conformance/src \
    crates/control-plane/src \
    crates/kernel-core/src \
    crates/node-core/src \
    crates/observatory-core/src \
    && printf "fn main() {}\n" > apps/wattetheria-cli/src/main.rs \
    && printf "fn main() {}\n" > apps/wattetheria-kernel/src/main.rs \
    && printf "fn main() {}\n" > apps/wattetheria-observatory/src/main.rs \
    && printf "pub fn _planner_stub() {}\n" > crates/conformance/src/lib.rs \
    && printf "pub fn _planner_stub() {}\n" > crates/control-plane/src/lib.rs \
    && printf "pub fn _planner_stub() {}\n" > crates/kernel-core/src/lib.rs \
    && printf "pub fn _planner_stub() {}\n" > crates/node-core/src/lib.rs \
    && printf "pub fn _planner_stub() {}\n" > crates/observatory-core/src/lib.rs

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

# Replace path deps in builder stage too (COPY . . overwrites the planner's sed).
RUN sed -i \
    -e 's|watt-did = { path = "../../../watt-did" }|watt-did = { git = "https://github.com/wattetheria/watt-did.git" }|' \
    -e 's|watt-wallet = { path = "../../../watt-wallet" }|watt-wallet = { git = "https://github.com/wattetheria/watt-wallet.git" }|' \
    crates/kernel-core/Cargo.toml \
    && printf '\n[patch."https://github.com/wattetheria/watt-wallet.git"]\nwatt-did = { git = "https://github.com/wattetheria/watt-did.git" }\n' \
    >> Cargo.toml

# Fetch wattswarm proto file for gRPC codegen (build.rs uses WATTSWARM_SYNC_PROTO).
ARG WATTSWARM_PROTO_REV=main
RUN curl -fsSL "https://raw.githubusercontent.com/wattetheria/wattswarm/${WATTSWARM_PROTO_REV}/apps/wattswarm/proto/wattetheria_sync.proto" \
    -o /tmp/wattetheria_sync.proto

ENV WATTSWARM_SYNC_PROTO=/tmp/wattetheria_sync.proto
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
