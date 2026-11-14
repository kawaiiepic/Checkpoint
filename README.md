# Checkpoint

A read-only Steam Deck and Linux Eglantine save watcher. **The Rust companion is the single detection engine**; Decky runs it in `--status-json` mode and renders notifications/status. No separate Python save scanner.

## Rust (Void Linux or Steam Deck)

```sh
cd companion
cargo build --release
./target/release/checkpoint-companion --status-json
./target/release/checkpoint-companion
```

The companion detects Eglantine AppImage processes and reads recently modified Eglantine boot logs for Switch 2 title IDs. It discovers save locations using Eglantine's verified legacy and current save layouts (see below). Nothing is uploaded, downloaded or modified.

## Decky

Build the Rust binary for the Steam Deck (Linux x86_64), place it at `bin/checkpoint-companion` in the plugin directory, then build the frontend:

```sh
mkdir -p bin
cp companion/target/release/checkpoint-companion bin/checkpoint-companion
pnpm install
pnpm build
```

Install the built plugin folder using Decky's developer plugin workflow. The Python backend starts the Rust binary to request JSON status every five seconds. The plugin requires `dist/`, `bin/checkpoint-companion`, `main.py` and `plugin.json`. This has not been tested on a physical Deck.

## Save discovery

Based on Eglantine's `src/core/file_sys/savedata_factory.cpp`, Checkpoint checks these **existing directories only**, for a booted Switch 2 title ID:

- `nand/user/save/account/<user-UUID>/<title-id-and-~0xFF>/0` (new account layout)
- `nand/user/save/device/<title-id-and-~0xFF>/0` (new device layout)
- `nand/user/save/0000000000000000/<32-hex-user-id>/<full-title-id>` (legacy layout)

Eglantine's data directory normally lives at `$XDG_DATA_HOME/eglantine` (or `~/.local/share/eglantine`), and portable installations can use a `user` directory beside the AppImage or in its current working directory. You may supply `--save-dir /path/to/nand/user/save` when using an unusual install.

Only nonempty save directories are reported. Discovery does not infer save ownership from file modification times and does not write files. Multiple profiles or storage layouts are reported separately. Title ID matching is safe for discovery, but future syncing must use content hashes, per-profile identity matching, backups, and conflicts.

If Eglantine's log is not in a default location, use `--log-file /path/to/eden_log.txt` with the CLI. The Decky bridge currently uses default log locations only.

## Limitations

- Save transfers are **not implemented**.
- No automatic account-profile equivalence between different machines.
- Unrecognized Eglantine sessions may show no title ID or save match.
- Steam library inline status injection is not yet implemented; notifications appear through Decky's toaster.
- Source hasn't been validated with a full Rust build in this environment.
