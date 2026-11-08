#!/usr/bin/env bash
# Build the launcher and its shipped plugins into dist/:
#   dist/kairos[.exe], dist/kairos.png
#   dist/plugins/<name>/{plugin.toml,kairos-<name>[.exe]}
# The launcher finds dist/plugins because it is next to the executable.
set -euo pipefail
cd "$(dirname "$0")/.."

exe=""
case "$(uname -s)" in MINGW*|MSYS*|CYGWIN*) exe=".exe" ;; esac

cargo build --release
rm -rf dist && mkdir -p dist/plugins
cp "target/release/kairos$exe" dist/
cp assets/icons/kairos-256.png dist/kairos.png

for dir in plugins/*/; do
    name=$(basename "$dir")
    cargo build --release --manifest-path "$dir/Cargo.toml"
    mkdir -p "dist/plugins/$name"
    cp "$dir/plugin.toml" "dist/plugins/$name/"
    cp "$dir/target/release/kairos-$name$exe" "dist/plugins/$name/"
done

ls -l dist dist/plugins/*
