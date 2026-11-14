#!/usr/bin/env bash
# Runs the CLI end to end against the Firebase emulators: builds the publish API, starts
# the emulators under the throwaway project id "demo-ctx" and runs the Rust e2e test.
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"

npm --prefix "$root/registry/functions" run build
cargo build --manifest-path "$root/Cargo.toml" -p ctx-cli

cd "$root/registry"
CTX_E2E=1 firebase emulators:exec \
  --project demo-ctx \
  --only auth,firestore,storage,functions \
  "cargo test --manifest-path '$root/Cargo.toml' -p ctx-cli --test e2e -- --nocapture" </dev/null
