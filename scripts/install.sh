#!/bin/sh
set -eu

script_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
install_dir=${1:-"${HOME:?HOME must be set}/.local/bin"}

cargo build --release --manifest-path "$script_dir/../Cargo.toml"
mkdir -p "$install_dir"
install -m 755 "$script_dir/../target/release/llmx" "$install_dir/llmx"

printf 'Installed llmx to %s/llmx\n' "$install_dir"
