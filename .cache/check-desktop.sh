set -eu
cd /workspace
rustfmt --edition 2021 --check apps/desktop/src/main.rs && echo 'main.rs fmt ok'
cargo test -p util-maker-desktop 2>&1 | tail -5