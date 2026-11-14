#!/usr/bin/env bash
# Deploys a registry to a Firebase project: security rules, the publish API and the web
# app, together with the descriptor that tells clients how to reach this registry.
#
#   scripts/deploy-registry.sh <firebase-project-id> [--only hosting]
#
# The descriptor is read from registry/descriptor.<project-id>.json (not committed; see
# spec/registry.schema.json for its shape).
set -euo pipefail

project="${1:?usage: scripts/deploy-registry.sh <firebase-project-id> [--only <targets>]}"
shift
only="firestore,storage,functions,hosting"
if [ "${1:-}" = "--only" ]; then
  only="${2:?--only needs a value}"
fi

root="$(cd "$(dirname "$0")/.." && pwd)"
descriptor="$root/registry/descriptor.$project.json"
[ -f "$descriptor" ] || { echo "missing $descriptor" >&2; exit 1; }

npm --prefix "$root/web" ci
npm --prefix "$root/web" run build
# Firebase Hosting only serves a directory inside registry/, so assemble the site there.
site="$root/registry/public"
rm -rf "$site"
cp -R "$root/web/dist" "$site"
mkdir -p "$site/.well-known"
cp "$descriptor" "$site/.well-known/ctx-registry.json"
cp "$root/install.sh" "$site/install.sh"

cd "$root/registry"
firebase deploy --project "$project" --only "$only" --non-interactive --force </dev/null
