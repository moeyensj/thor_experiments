#!/usr/bin/env bash
# Extract THOR at the commit pinned in thor.rev into vendor/thor-rs, which Cargo.toml depends on.
#   ./fetch_thor.sh [path to a thor_rust clone]
# Without a path, it clones git@github.com:moeyensj/thor_rust.git into vendor/thor_rust.git (bare).
set -euo pipefail
cd "$(dirname "$0")"
rev=$(tr -d '[:space:]' < thor.rev)
src=${1:-}
if [ -z "$src" ]; then
  src=vendor/thor_rust.git
  [ -d "$src" ] || git clone --bare --filter=blob:none git@github.com:moeyensj/thor_rust.git "$src"
  git -C "$src" fetch -q origin "$rev" 2>/dev/null || true
fi
rm -rf vendor/thor-rs && mkdir -p vendor/thor-rs
git -C "$src" archive "$rev" | tar -x -C vendor/thor-rs
echo "$rev" > vendor/thor-rs/.thor-rev
echo "THOR $rev extracted to vendor/thor-rs"
