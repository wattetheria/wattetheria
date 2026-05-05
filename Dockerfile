FROM rust:1.93-bookworm AS chef

WORKDIR /app

ENV CARGO_NET_GIT_FETCH_WITH_CLI=true

RUN cargo install cargo-chef --locked

FROM chef AS dev

RUN cargo install cargo-watch --locked

FROM chef AS planner

COPY Cargo.toml Cargo.lock ./
COPY apps/wattetheria-cli/Cargo.toml apps/wattetheria-cli/Cargo.toml
COPY apps/wattetheria-kernel/Cargo.toml apps/wattetheria-kernel/Cargo.toml
COPY crates/conformance/Cargo.toml crates/conformance/Cargo.toml
COPY crates/control-plane/Cargo.toml crates/control-plane/Cargo.toml
COPY crates/gateway-contract/Cargo.toml crates/gateway-contract/Cargo.toml
COPY crates/kernel-core/Cargo.toml crates/kernel-core/Cargo.toml
COPY crates/node-core/Cargo.toml crates/node-core/Cargo.toml
COPY crates/social/Cargo.toml crates/social/Cargo.toml

# Replace local path dependencies with git sources for Docker builds.
# This lets users build the image without cloning watt-did / watt-wallet.
# 1) kernel-core + control-plane + social: swap path deps to git
RUN sed -i \
    -e 's|watt-did = { path = "../../../watt-did" }|watt-did = { git = "https://github.com/wattetheria/watt-did.git" }|' \
    -e 's|watt-wallet = { path = "../../../watt-wallet" }|watt-wallet = { git = "https://github.com/wattetheria/watt-wallet.git" }|' \
    crates/kernel-core/Cargo.toml \
    && sed -i \
    -e 's|watt-wallet = { path = "../../../watt-wallet" }|watt-wallet = { git = "https://github.com/wattetheria/watt-wallet.git" }|' \
    crates/control-plane/Cargo.toml \
    && sed -i \
    -e 's|watt-did = { path = "../../../watt-did" }|watt-did = { git = "https://github.com/wattetheria/watt-did.git" }|' \
    crates/social/Cargo.toml
# 2) root Cargo.toml: patch watt-wallet's internal path dep on watt-did
RUN printf '\n[patch."https://github.com/wattetheria/watt-wallet.git"]\nwatt-did = { git = "https://github.com/wattetheria/watt-did.git" }\n' \
    >> Cargo.toml
# 3) root Cargo.toml: remove local wattswarm path override inside Docker.
RUN sed -i \
    '/^\[patch\."https:\/\/github\.com\/wattetheria\/wattswarm\.git"\]$/,+1d' \
    Cargo.toml

RUN mkdir -p \
    apps/wattetheria-cli/src \
    apps/wattetheria-kernel/src \
    crates/conformance/src \
    crates/control-plane/src \
    crates/gateway-contract/src \
    crates/kernel-core/src \
    crates/node-core/src \
    crates/social/src \
    && printf "fn main() {}\n" > apps/wattetheria-cli/src/main.rs \
    && printf "fn main() {}\n" > apps/wattetheria-kernel/src/main.rs \
    && printf "pub fn _planner_stub() {}\n" > crates/conformance/src/lib.rs \
    && printf "pub fn _planner_stub() {}\n" > crates/control-plane/src/lib.rs \
    && printf "pub fn _planner_stub() {}\n" > crates/gateway-contract/src/lib.rs \
    && printf "pub fn _planner_stub() {}\n" > crates/kernel-core/src/lib.rs \
    && printf "pub fn _planner_stub() {}\n" > crates/node-core/src/lib.rs \
    && printf "pub fn _planner_stub() {}\n" > crates/social/src/lib.rs

RUN cargo chef prepare --recipe-path recipe.json

FROM chef AS cacher

COPY --from=planner /app/recipe.json /app/recipe.json
RUN --mount=type=cache,target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,target=/usr/local/cargo/git,sharing=locked \
    --mount=type=secret,id=github_token \
    if [ -f /run/secrets/github_token ]; then \
      git config --global url."https://$(cat /run/secrets/github_token)@github.com/".insteadOf "https://github.com/"; \
    fi \
    && cargo chef cook --release --recipe-path recipe.json \
    -p wattetheria-kernel \
    && rm -f /root/.gitconfig

FROM chef AS builder

COPY . .
COPY --from=cacher /app/target /app/target

# Replace path deps in builder stage too (COPY . . overwrites the planner's sed).
RUN sed -i \
    -e 's|watt-did = { path = "../../../watt-did" }|watt-did = { git = "https://github.com/wattetheria/watt-did.git" }|' \
    -e 's|watt-wallet = { path = "../../../watt-wallet" }|watt-wallet = { git = "https://github.com/wattetheria/watt-wallet.git" }|' \
    crates/kernel-core/Cargo.toml \
    && sed -i \
    -e 's|watt-wallet = { path = "../../../watt-wallet" }|watt-wallet = { git = "https://github.com/wattetheria/watt-wallet.git" }|' \
    crates/control-plane/Cargo.toml \
    && sed -i \
    -e 's|watt-did = { path = "../../../watt-did" }|watt-did = { git = "https://github.com/wattetheria/watt-did.git" }|' \
    crates/social/Cargo.toml \
    && sed -i \
    '/^\[patch\."https:\/\/github\.com\/wattetheria\/wattswarm\.git"\]$/,+1d' \
    Cargo.toml \
    && printf '\n[patch."https://github.com/wattetheria/watt-wallet.git"]\nwatt-did = { git = "https://github.com/wattetheria/watt-did.git" }\n' \
    >> Cargo.toml

# Fetch wattswarm proto file for gRPC codegen (build.rs uses WATTSWARM_SYNC_PROTO).
ARG WATTSWARM_PROTO_REV=main
RUN --mount=type=secret,id=github_token \
    TOKEN=$(cat /run/secrets/github_token 2>/dev/null || echo "") \
    && curl -fsSL \
      -H "Authorization: token ${TOKEN}" \
      "https://raw.githubusercontent.com/wattetheria/wattswarm/${WATTSWARM_PROTO_REV}/apps/wattswarm/proto/wattetheria_sync.proto" \
      -o /tmp/wattetheria_sync.proto

ENV WATTSWARM_SYNC_PROTO=/tmp/wattetheria_sync.proto
RUN --mount=type=cache,target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,target=/usr/local/cargo/git,sharing=locked \
    --mount=type=secret,id=github_token \
    if [ -f /run/secrets/github_token ]; then \
      git config --global url."https://$(cat /run/secrets/github_token)@github.com/".insteadOf "https://github.com/"; \
    fi \
    && cargo build --release -p wattetheria-kernel \
    && rm -f /root/.gitconfig

FROM debian:bookworm-slim

RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates curl \
    && rm -rf /var/lib/apt/lists/*

RUN useradd --create-home --uid 10001 wattetheria

WORKDIR /app

COPY --from=builder /app/target/release/wattetheria-kernel /app/target/release/wattetheria-kernel
COPY --from=builder /app/scripts/docker-kernel-entrypoint.sh /app/scripts/docker-kernel-entrypoint.sh

RUN mkdir -p /var/lib/wattetheria \
    && chmod +x /app/scripts/docker-kernel-entrypoint.sh \
    && chown -R wattetheria:wattetheria /var/lib/wattetheria /app

USER wattetheria

EXPOSE 7777

ENTRYPOINT ["/app/scripts/docker-kernel-entrypoint.sh"]
