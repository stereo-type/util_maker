#!/bin/sh
set -eu
rustc --version
cargo --version
cargo clippy --version
cargo fmt --version
node --version
npm --version
tsc --version
tauri --version
pkg-config --modversion webkit2gtk-4.1 ayatana-appindicator3-0.1 librsvg-2.0 openssl
work_dir=$(mktemp -d)
trap 'rm -rf "$work_dir"' EXIT
cd "$work_dir"
cargo init --quiet --name check_env --vcs none
cargo test --offline --quiet
printf '%s\n' 'Linux development environment ready.'
