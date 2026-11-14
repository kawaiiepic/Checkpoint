# Rust terminal companion

This is the Linux desktop companion for Checkpoint. It is a standalone Rust CLI for Void Linux and other distributions. It does not depend on Decky, systemd, or a GUI.

```sh
cd companion
cargo build --release
./target/release/checkpoint-companion --save-dir "$HOME/.local/share/eglantine/nand/user/save"
```

Use `--help` for flags. Optional `--remote mycloud:Checkpoint/eglantine` uses rclone to **check remote file listings only**. `--notify` also uses `notify-send`; otherwise notifications print in the terminal. Default polling: 30 seconds while Eglantine is running, 30 seconds when idle. You can override these with `--active-seconds` and `--idle-seconds`, or run `--once`.

This is a **read-only prototype**. It does not upload or download saves. It detects Eglantine processes, identifies directly launched ROMs through process arguments and observes configured save-folder changes. When launching games within the Eglantine GUI, the active ROM may be unknown. Save folders are not yet reliably mapped to individual title IDs. Set `--save-dir` explicitly if the automatic candidates do not match your actual installation. No remote listing change is treated as proof that a newer compatible save exists.

The source has not been compiled on a Rust-equipped machine in this environment.

### AppImage process detection

The companion detects Eglantine AppImages using executable paths, command lines, or the `APPIMAGE` process environment when available. It does not inspect open file handles. Launching an individual title inside the emulator GUI may still show an unknown ROM.

### Extracted games and .nup files

Checkpoint recognizes directly launched `.nup` files as well as `.xci`, `.nsp`, `.nca`, and `.nro`. It also recognizes extracted games supplied as an `exefs` directory or `exefs/main` file, provided that `exefs/main` exists on disk. For layouts with a numeric intermediate directory (such as `131072/exefs`), it uses the enclosing directory name as a provisional game label. Detection is based on launch arguments; it is not a verified title-ID lookup or save-folder association.

### Detecting the active game through Eglantine logs

Checkpoint can now read the newest `BootGame: Booting game: TITLE_ID | TITLE |` event while Eglantine is running, so a game opened inside the emulator GUI can be identified without a ROM command-line argument. The title ID is recorded alongside Eglantine's reported title, for example `04007EF01F8AE000 | FEFW ROM (64-bit)`. It scans common `~/.local/share/{eglantine,Eglantine,eden,Eden}/log/{eden_log,eglantine_log}.txt` paths and supports `--log-file /path/to/eden_log.txt` for portable installations. It ignores logs that were not modified recently on first discovery, to reduce stale-game attribution. This is detection only; it does not associate the title ID with a save directory or transfer files. A 30-second polling interval still applies.
