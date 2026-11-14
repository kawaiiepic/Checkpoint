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

## Automatic cloud backups (experimental)

Configure **one global destination** for all games. No individual game settings are needed. The Rust companion automatically discovers all installed Switch 2 saves and stores independent snapshots under `switch2/<title-id>/<layout>/<profile-id>/<machine-id>/snapshots/<timestamp>-<signature>`.

For WebDAV, first configure an rclone WebDAV remote using `rclone config`, then run:

```sh
checkpoint-companion --configure-sync rclone 'mywebdav:Checkpoint'
```

For rsync over SSH, configure SSH authentication and run:

```sh
checkpoint-companion --configure-sync rsync 'myserver:/backups/Checkpoint'
```

One global file is stored at `~/.config/checkpoint/config.json` (or `$CHECKPOINT_CONFIG`). `--backup-once` invokes a safe idle-only backup. Normal terminal monitoring attempts backups while Eglantine is stopped; Decky invokes the same Rust command about every 30 seconds while idle. Source saves are never deleted, and remote snapshot paths are never reused intentionally. This feature **backs up** saves; it does NOT yet download/restore to another device, merge concurrent changes or resolve profile identities between devices. Remote storage grows with snapshots until retention support is introduced. Requires `rclone` or `rsync` installed and authenticated. Check available storage before using it. Avoid running concurrent Checkpoint instances with the same global configuration.

## Native Nextcloud WebDAV (no rclone or rsync)

Run the following in the Rust companion directory:

```sh
./target/release/checkpoint-companion --configure-webdav \
  'https://cloud.blossomvale.dev/remote.php/dav/files/USERNAME/Checkpoint' USERNAME
```

Use the exact WebDAV URL from Nextcloud **Files settings**, adding the `Checkpoint` directory to the end. Checkpoint prompts privately for a dedicated Nextcloud **app password** (the password won't be an argument in shell history). It verifies access before saving one global configuration file at `~/.config/checkpoint/config.json` using owner-only file permissions (`0600` on Linux). **The app password is stored in plaintext in this protected local file**. Secure access to the host, account, and any backups of the configuration file.

Check connectivity: `./target/release/checkpoint-companion --test-webdav`. Run `--backup-once` with Eglantine closed to upload discovered Switch 2 saves directly over HTTPS. The backend creates directories using `MKCOL`, uploads files using `PUT` with overwrite protection, and writes a `.checkpoint-complete` marker only after a snapshot's files upload successfully. Traefik, Nextcloud, and intermediaries must accept WebDAV methods and sufficient request body sizes.

There is still **one global destination**, no per-game configuration, and **no automatic restore or conflict resolution**. Existing rclone and rsync backends remain supported.
