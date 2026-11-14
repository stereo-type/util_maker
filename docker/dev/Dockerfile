FROM node:24.22.3-bookworm-slim@sha256:dc26ae92c9fb15cf6b1d63e20f0944b2efe0f115b20e00be48fb23fceb061c54 AS node
RUN npm install --global --no-audit --no-fund @tauri-apps/cli@2.12.4 typescript@5.9.3

FROM rust:1.100.0-bookworm@sha256:ade39a710ef85f3506cffcfec3e8a24ba090badef1dff2fb798f71d878833fea
RUN apt-get update && apt-get install -y --no-install-recommends \
        build-essential ca-certificates curl file git make pkg-config ripgrep \
        libayatana-appindicator3-dev librsvg2-dev libssl-dev libwebkit2gtk-4.1-dev libxdo-dev \
    && rm -rf /var/lib/apt/lists/*
RUN rustup component add clippy rustfmt
COPY --from=node /usr/local/bin/node /usr/local/bin/node
COPY --from=node /usr/local/lib/node_modules /usr/local/lib/node_modules
RUN ln -s ../lib/node_modules/npm/bin/npm-cli.js /usr/local/bin/npm \
    && ln -s ../lib/node_modules/npm/bin/npx-cli.js /usr/local/bin/npx \
    && ln -s ../lib/node_modules/@tauri-apps/cli/tauri.js /usr/local/bin/tauri \
    && ln -s ../lib/node_modules/typescript/bin/tsc /usr/local/bin/tsc
COPY check-env.sh /usr/local/bin/util-maker-check-env
WORKDIR /workspace
