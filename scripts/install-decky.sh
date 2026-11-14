#!/usr/bin/env bash
# Build and install Checkpoint into a local Steam Deck's Decky Loader.
set -Eeuo pipefail

ROOT="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

if [[ "$(uname -s)" != "Linux" || "$(uname -m)" != "x86_64" ]]; then
  echo "Checkpoint Decky installer requires Linux x86_64 (build on the Steam Deck)." >&2
  exit 1
fi
for cmd in cargo pnpm sudo systemctl python3; do
  if ! command -v "$cmd" >/dev/null 2>&1; then
    echo "Missing ${cmd}. Install it and retry." >&2
    exit 1
  fi
done

DECK_HOME="${DECK_HOME:-/home/deck}"
DEST="$DECK_HOME/homebrew/plugins/Checkpoint"
if [[ ! -d "$DECK_HOME/homebrew/plugins" ]]; then
  echo "Decky plugin directory not found at $DECK_HOME/homebrew/plugins." >&2
  echo "Install Decky Loader first, or set DECK_HOME to the Deck user home." >&2
  exit 1
fi
if ! systemctl list-unit-files plugin_loader.service --no-legend --no-pager 2>/dev/null | grep -q '^plugin_loader.service'; then
  echo "Warning: plugin_loader.service not found. Check your Decky Loader installation." >&2
fi

echo "[Checkpoint] Building Rust companion..."
cargo build --release --manifest-path companion/Cargo.toml

echo "[Checkpoint] Building Decky frontend..."
pnpm install --frozen-lockfile
pnpm build

test -s dist/index.js || { echo "Missing frontend bundle dist/index.js" >&2; exit 1; }
test -x companion/target/release/checkpoint-companion || {
  echo "Missing executable companion/target/release/checkpoint-companion" >&2; exit 1;
}
test -s main.py && test -s plugin.json && test -s package.json
python3 -m json.tool plugin.json >/dev/null
python3 -m json.tool package.json >/dev/null

# Current Decky Loader imports modern plugin bundles as ES modules.
# A legacy loader that eval()s them produces "Unexpected token 'export'".
if ! grep -Eq 'export[[:space:]]*\\{|export[[:space:]]+default' dist/index.js; then
  echo "Warning: frontend does not appear to be an ES module; check @decky/rollup output." >&2
fi

echo "[Checkpoint] Installing into $DEST..."
sudo install -d -m 755 "$DEST" "$DEST/dist" "$DEST/bin"
sudo install -m 644 dist/index.js "$DEST/dist/index.js"
sudo install -m 755 companion/target/release/checkpoint-companion "$DEST/bin/checkpoint-companion"
sudo install -m 644 main.py plugin.json package.json "$DEST/"
# Ensure any other frontend assets emitted by Rollup are included.
sudo cp -a dist/. "$DEST/dist/"

echo "[Checkpoint] Restarting Decky Loader..."
sudo systemctl restart plugin_loader.service

echo "[Checkpoint] Installed! Open the Quick Access menu -> Decky -> Checkpoint."
echo "[Checkpoint] If Decky shows 'Unexpected token export', update Decky Loader."
echo "[Checkpoint] Old Decky versions eval ES modules instead of importing them."
echo "[Checkpoint] Log: $DECK_HOME/homebrew/logs/Checkpoint/"
