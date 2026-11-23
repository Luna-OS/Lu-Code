# syntax=docker/dockerfile:1.10
# luCode – Multi-Stage-Build für amd64 und arm64: Oberfläche (Dioxus/WebAssembly) und Programm
# (Rust). Braucht nur Docker – kein Rust auf dem Rechner.
#   docker build -t lucode .
#   docker run -p 8510:8510 -p 8511:8511 -v lucode-data:/data lucode
# Nur das Programm (für Umbrel: landet in dist/linux_<arch>/):
#   docker buildx build --platform linux/amd64,linux/arm64 --target dist --output type=local,dest=dist .

ARG NODE=node:22-bookworm@sha256:18a6d90fd60542269850c80ca03b29b1d9b14d6b221ac7ee8e35b0d33ef904bd

ARG RUST=rust:1.98-bookworm@sha256:93ce27a88655056a51dbdd8f5f2d7ddc071c7b0070fb288a37b5a285fc83971e

# Oberfläche: dx lädt ein fertiges wasm-bindgen, das glibc 2.38 braucht – darum Debian Trixie.
ARG RUST_UI=rust:1.98-trixie@sha256:a8a5f0a1e5fe7dfe1d352591e4a1c7dd2c08fd70475cae872cf3458ba0df0546

# ---------------------------------------------------------------- Oberfläche
# WebAssembly ist überall gleich – einmal auf dem Build-Rechner bauen, für alle Architekturen.
FROM --platform=$BUILDPLATFORM ${RUST_UI} AS ui
RUN rustup target add wasm32-unknown-unknown
RUN --mount=type=cache,target=/usr/local/cargo/registry,sharing=locked \
    cargo install dioxus-cli --version 0.7.10 --locked --root /opt/dx
WORKDIR /src
COPY shared shared
COPY ui ui
RUN --mount=type=cache,target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,target=/src/ui/target,id=lucode-ui-target \
    cd ui && rm -rf target/dx/lucode-ui/release/web/public \
 && PATH=/opt/dx/bin:$PATH dx bundle --web --release --out-dir /tmp/ui \
 && mkdir -p /out && cp -r /tmp/ui/public /out/public && test -f /out/public/index.html

# ---------------------------------------------------------------- Programm
FROM --platform=$BUILDPLATFORM ${RUST} AS build
ARG TARGETARCH
ARG BUILDARCH
# Querübersetzer, wenn Ziel und Build-Rechner verschieden sind (SQLite und Krypto-Teile
# werden mit dem C-Compiler des Ziels gebaut).
RUN set -eu; \
    case "$TARGETARCH" in \
      amd64) t=x86_64-unknown-linux-gnu; p=x86_64-linux-gnu; pkg="gcc-x86-64-linux-gnu g++-x86-64-linux-gnu libc6-dev-amd64-cross" ;; \
      arm64) t=aarch64-unknown-linux-gnu; p=aarch64-linux-gnu; pkg="gcc-aarch64-linux-gnu g++-aarch64-linux-gnu libc6-dev-arm64-cross" ;; \
      *) echo "Architektur $TARGETARCH nicht unterstützt" >&2; exit 1 ;; \
    esac; \
    echo "$t" > /rust-target; \
    if [ "$TARGETARCH" != "$BUILDARCH" ]; then \
      apt-get update && apt-get install -y --no-install-recommends $pkg && rm -rf /var/lib/apt/lists/*; \
      rustup target add "$t"; \
      printf '[target.%s]\nlinker = "%s-gcc"\n' "$t" "$p" >> /usr/local/cargo/config.toml; \
      u=$(echo "$t" | tr - _); \
      printf 'CC_%s=%s-gcc\nCXX_%s=%s-g++\nAR_%s=%s-ar\n' "$u" "$p" "$u" "$p" "$u" "$p" > /cross-env; \
    else \
      touch /cross-env; \
    fi
WORKDIR /src
COPY Cargo.toml Cargo.lock build.rs Rocket.toml ./
COPY shared shared
COPY migrations migrations
COPY src src
COPY static static
COPY locales locales
# Oberfläche aus der Stufe oben; build.rs bettet sie ins Programm ein (Text und WebAssembly
# gzip) – der Server braucht dafür keine Dateien mehr.
COPY --from=ui /out/public ui/dist/public
RUN --mount=type=cache,target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,target=/src/target,id=lucode-target-$TARGETARCH \
    env $(cat /cross-env) cargo build --release --locked --target "$(cat /rust-target)" \
 && cp "target/$(cat /rust-target)/release/lucode" /lucode

FROM scratch AS dist
COPY --from=build /lucode /lucode
COPY Rocket.toml /Rocket.toml

# ---------------------------------------------------------------- Laufzeit
# node:22-bookworm bringt Node/npm (für die Agent-CLIs, die luCode in /data/tools installiert),
# git, curl und Compiler mit – Agenten können damit Projekte bauen und testen.
FROM ${NODE} AS runtime
RUN apt-get update \
 && apt-get install -y --no-install-recommends openssh-client ripgrep jq \
 && rm -rf /var/lib/apt/lists/*
COPY --from=build /lucode /usr/local/bin/lucode
COPY Rocket.toml /app/Rocket.toml

WORKDIR /app
ENV LUCODE_CONFIG=/app/Rocket.toml \
    LUCODE_DATA_DIR=/data \
    LUCODE_ADDRESS=0.0.0.0 \
    LUCODE_PORT=8510 \
    LUCODE_PREVIEW_PORT=8511 \
    LUCODE_LOG=info,rocket::server=warn,rocket::launch::_=warn,rocket::shield=warn
VOLUME /data
EXPOSE 8510 8511
HEALTHCHECK --interval=30s --timeout=5s --start-period=20s --retries=3 \
  CMD ["lucode", "healthcheck"]
STOPSIGNAL SIGTERM
CMD ["lucode"]
