# Checkpoint

A read-only Decky Loader prototype for detecting locally updated Eglantine game saves on Steam Deck.

## Current scope

- Detects running Eglantine / `eden-cli` processes launched with a ROM argument.
- Watches selected Eglantine save folders for file metadata changes.
- Displays a Decky toast when a local save file changes and settles.
- Shows detection status in Decky's Quick Access Menu.
- Does **not** sync, upload, download, or modify saves. It does not currently add a native Steam library status row.

## Development

Requires a Steam Deck with Decky Loader, Node.js and pnpm for frontend builds.

```sh
pnpm install
pnpm build
```

Install the built plugin directory using Decky Loader's developer plugin installation workflow. The `dist/` output is required for the frontend. This source release has not been validated on a physical Steam Deck.

## Eglantine save directories

The prototype checks several common Eglantine data directories beneath the Deck user's home directory. If yours differs, the backend supports a colon-separated `EGLANTINE_SAVE_DIRS` environment variable. This environment variable must be passed to the Decky backend, not just an interactive shell.

Eglantine must be launched with a ROM path (`eglantine /path/to/game.xci` or `--game /path/to/game.xci`) for reliable game-name recognition in this prototype. Starting a game from Eglantine's own library after launching its GUI is not yet supported.

The watcher tracks file metadata across scanned roots, not precise per-title save ownership. A notification is attributed to the active ROM only when exactly one recognizable Eglantine game process exists. Verify your actual save path in the plugin status page.

## Safety

Read-only: no cloud integration or save writes. Don't interpret a local save notification as a newer remote/cloud save.
