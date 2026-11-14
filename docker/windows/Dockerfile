# Кросс-сборка установщика Windows x64 из Ubuntu.
#
# Образ содержит Rust, LLVM/LLD (через clang), cargo-xwin и NSIS. Сборка
# запускается целью `make build-windows`; результат — dist/UtilMaker-Setup-*.exe.

FROM rust:1.100.0-bookworm

RUN apt-get update && apt-get install -y --no-install-recommends \
        ca-certificates clang cmake curl git lld llvm make nsis pkg-config xz-utils zip \
    && rm -rf /var/lib/apt/lists/*

RUN rustup target add x86_64-pc-windows-msvc \
    && cargo install cargo-xwin --locked

COPY build-windows.sh /usr/local/bin/util-maker-build-windows
RUN chmod 0755 /usr/local/bin/util-maker-build-windows

WORKDIR /workspace
