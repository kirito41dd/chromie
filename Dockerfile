FROM rust:1.99.1-bookworm AS base
ARG TARGETARCH
WORKDIR /build
ENV CARGO_BUILD_JOBS=4
RUN apt-get update && apt-get install -y --no-install-recommends python3 && rm -rf /var/lib/apt/lists/*

FROM base AS source
COPY Cargo.toml Cargo.lock ./
COPY LICENSE THIRD_PARTY_NOTICES.md ./
COPY licenses ./licenses
COPY src ./src
COPY migration ./migration
COPY web ./web
COPY integrations ./integrations
COPY examples/codex/secret-curl.py ./examples/codex/secret-curl.py
COPY scripts ./scripts

# Linux CLI 单独导出，保持 bookworm 的 glibc 基线；不依赖宿主 runner 的发行版。
FROM source AS client-builder
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,id=chromie-target-bookworm-${TARGETARCH},target=/build/target \
    cargo build --locked --release -p chromie --no-default-features --features client --bin chromie && \
    mkdir -p /out && cp target/release/chromie /out/chromie && strip /out/chromie

FROM scratch AS client-export
COPY --from=client-builder /out/chromie /chromie

FROM base AS builder
RUN rustup target add wasm32-unknown-unknown
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    cargo install wasm-pack --version 0.15.0 --locked
COPY --from=source /build /build
COPY --from=client-builder /out/chromie /out/chromie
COPY packaging/clients /extra-clients
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,id=chromie-target-bookworm-${TARGETARCH},target=/build/target \
    cargo build --locked --release -p chromie --no-default-features --features server --bin chromie-server && \
    mkdir -p /out && cp target/release/chromie-server /out/chromie-server && strip /out/chromie-server && \
    mkdir -p /out/web && \
    wasm-pack build web --release --target web --out-dir /out/web --no-pack --no-opt -- --locked && \
    python3 scripts/build-cli.py --binary /out/chromie --output /out/web/cli && \
    python3 scripts/build-codex-plugin.py --binary /out/chromie --output /out/web/chromie-codex.zip --client-output /out/web/chromie-client.zip && \
    cp -a /extra-clients/. /out/web/cli/ && \
    rm -f /out/web/cli/.gitkeep

FROM debian:trixie-slim AS runtime
ARG CHROMIE_VERSION=0.1.0
ARG CHROMIE_REVISION
LABEL org.opencontainers.image.title="Chromie" \
      org.opencontainers.image.version="${CHROMIE_VERSION}" \
      org.opencontainers.image.source="https://github.com/kirito41dd/chromie" \
      org.opencontainers.image.revision="${CHROMIE_REVISION}" \
      org.opencontainers.image.description="个人记忆与加密 Secret 服务"
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates && rm -rf /var/lib/apt/lists/*
COPY --from=builder /out/chromie-server /usr/local/bin/chromie-server
COPY --from=builder /out/web /opt/chromie/web
COPY LICENSE THIRD_PARTY_NOTICES.md /opt/chromie/licenses/
COPY licenses /opt/chromie/licenses/licenses
COPY web/fonts/OFL.txt /opt/chromie/licenses/web/fonts/OFL.txt
COPY web/fonts/README.md /opt/chromie/licenses/web/fonts/README.md
ENV CHROMIE_WEB_DIR=/opt/chromie/web
ENV CHROMIE_CONFIG_DIR=/config
WORKDIR /opt/chromie
EXPOSE 8732
STOPSIGNAL SIGTERM
ENTRYPOINT ["chromie-server"]
CMD ["container-serve"]
