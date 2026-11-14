"""Checkpoint Decky interface; all detection and save discovery run in Rust."""
import asyncio
import json
import os
from pathlib import Path

import decky

HERE = Path(__file__).resolve().parent
BINARIES = [
    HERE / "bin" / "checkpoint-companion",
    HERE / "companion" / "target" / "release" / "checkpoint-companion",
    Path("/usr/local/bin/checkpoint-companion"),
]
POLL_SECONDS = 5
# Decky services may execute as root; inspect the Steam Deck user's data.
DECK_HOME = Path("/home/deck")
RUST_ENV = {**os.environ, "HOME": str(DECK_HOME)}
RUST_ENV["XDG_DATA_HOME"] = str(DECK_HOME / ".local/share")
RUST_ENV["XDG_CONFIG_HOME"] = str(DECK_HOME / ".config")
RUST_ENV["XDG_STATE_HOME"] = str(DECK_HOME / ".local/state")
RUST_ENV["XDG_CACHE_HOME"] = str(DECK_HOME / ".cache")

class Plugin:
    async def _main(self):
        self.status = {
            "running": False, "game": None, "title_id": None,
            "save_roots": [], "saves": [], "watched_files": 0, "process_count": 0,
        }
        self.error = None
        self._task = asyncio.create_task(self._poll())
        decky.logger.info("Checkpoint Rust status bridge started")

    async def _unload(self):
        self._task.cancel()
        try:
            await self._task
        except asyncio.CancelledError:
            pass

    async def get_status(self):
        return {**self.status, "error": self.error}

    async def _poll(self):
        last_session = None
        last_signatures = None
        backup_counter = 0
        while True:
            try:
                binary = next((path for path in BINARIES if path.is_file() and os.access(path, os.X_OK)), None)
                if binary is None:
                    raise FileNotFoundError("Build the Rust companion and copy it to bin/checkpoint-companion")
                process = await asyncio.create_subprocess_exec(
                    str(binary), "--status-json",
                    env=RUST_ENV,
                    stdout=asyncio.subprocess.PIPE,
                    stderr=asyncio.subprocess.PIPE,
                )
                try:
                    out, err = await asyncio.wait_for(process.communicate(), timeout=12)
                except asyncio.TimeoutError:
                    process.kill()
                    await process.communicate()
                    raise RuntimeError("Rust status scan timed out")
                if process.returncode:
                    raise RuntimeError(err.decode(errors="replace").strip() or f"Rust exited {process.returncode}")
                current = json.loads(out)
                session = (current.get("running"), current.get("title_id"))
                signatures = {save["path"]: save["signature"] for save in current.get("saves", [])}
                if session != last_session:
                    if current.get("running") and current.get("title_id"):
                        await decky.emit("game_detected", {
                            "game": current.get("game"), "title_id": current.get("title_id")
                        })
                    last_signatures = signatures
                elif last_signatures is not None and signatures != last_signatures and current.get("title_id"):
                    await decky.emit("save_updated", {
                        "game": current.get("game"), "title_id": current.get("title_id"),
                        "changed_count": sum(last_signatures.get(path) != value for path, value in signatures.items())
                    })
                    last_signatures = signatures
                self.status = current
                self.error = None
                # Every ~30 seconds while idle. Rust checks again immediately before copying.
                if not current.get("running"):
                    backup_counter += 1
                    if backup_counter >= 6:
                        backup_counter = 0
                        backup = await asyncio.create_subprocess_exec(
                            str(binary), "--backup-once",
                            env=RUST_ENV,
                            stdout=asyncio.subprocess.PIPE, stderr=asyncio.subprocess.PIPE,
                        )
                        try:
                            _, backup_err = await asyncio.wait_for(backup.communicate(), timeout=90)
                        except asyncio.TimeoutError:
                            backup.kill()
                            await backup.communicate()
                            raise RuntimeError("Automatic backup timed out")
                        if backup.returncode:
                            decky.logger.warning("Checkpoint backup: %s", backup_err.decode(errors="replace"))
                else:
                    backup_counter = 0
                last_session = session
            except asyncio.CancelledError:
                raise
            except Exception as exc:
                self.error = str(exc)
                decky.logger.warning("Checkpoint Rust bridge: %s", exc)
                last_session = None
                last_signatures = None
            await asyncio.sleep(POLL_SECONDS)
