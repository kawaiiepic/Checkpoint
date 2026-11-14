# Rust terminal companion

This is the Linux desktop companion for Checkpoint. It is a standalone Rust CLI for Void Linux and other distributions. It does not depend on Decky, systemd, or a GUI.

```sh
cd companion
cargo build --release
./target/release/checkpoint-companion --save-dir "$HOME/.local/share/eglantine/nand/user/save"
```

Use `--help` for flags. Optional `--remote mycloud:Checkpoint/eglantine` uses rclone to **check remote file listings only**. `--notify` also uses `notify-send`; otherwise notifications print in the terminal. Default polling: 30 seconds while Eglantine is running, 180 seconds when idle. You can override these with `--active-seconds` and `--idle-seconds`, or run `--once`.

This is a **read-only prototype**. It does not upload or download saves. It detects Eglantine processes, identifies directly launched ROMs through process arguments and observes configured save-folder changes. When launching games within the Eglantine GUI, the active ROM may be unknown. Save folders are not yet reliably mapped to individual title IDs. Set `--save-dir` explicitly if the automatic candidates do not match your actual installation. No remote listing change is treated as proof that a newer compatible save exists.

The source has not been compiled on a Rust-equipped machine in this environment.
