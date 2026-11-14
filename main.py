"""Checkpoint Decky backend: read-only Eglantine save-change watcher."""
import asyncio
import datetime
import os
from pathlib import Path
import time

import decky

HOME = Path(decky.DECKY_USER_HOME)
ROOT_CANDIDATES = [
    HOME / ".local/share/eglantine/nand/user/save",
    HOME / ".local/share/Eglantine/nand/user/save",
    HOME / ".local/share/eglantine/user/nand/user/save",
]
SCAN_INTERVAL = 3.0
SETTLE_SECONDS = 6.0
MAX_FILES = 30000


def _game_from_args(args):
    for i, arg in enumerate(args):
        value = arg.split("=", 1)[1] if arg.startswith("--game=") else None
        if arg in ("--game", "-g") and i + 1 < len(args):
            value = args[i + 1]
        if value is None and arg.lower().endswith((".nsp", ".xci", ".nca", ".nro")):
            value = arg
        if value and value.lower().endswith((".nsp", ".xci", ".nca", ".nro")):
            return Path(value).stem
    return None


def _read_processes():
    games = []
    try:
        for proc in Path("/proc").iterdir():
            if not proc.name.isdigit():
                continue
            try:
                raw = (proc / "cmdline").read_bytes().split(b"\0")
                args = [arg.decode("utf-8", "replace") for arg in raw if arg]
                if not args:
                    continue
                executable = os.path.basename(args[0]).lower()
                if not (executable.startswith("eglantine") or executable == "eden-cli"):
                    continue
                game = _game_from_args(args)
                if game:
                    games.append((int(proc.name), game))
            except (OSError, ValueError):
                continue
    except OSError:
        pass
    return games


def _roots():
    candidates = list(ROOT_CANDIDATES)
    candidates.extend(
        Path(value).expanduser()
        for value in os.environ.get("EGLANTINE_SAVE_DIRS", "").split(":")
        if value.strip()
    )
    return list(dict.fromkeys(path.resolve() for path in candidates if path.is_dir()))


def _snapshot(roots):
    files = {}
    for root in roots:
        stack = [root]
        while stack and len(files) < MAX_FILES:
            directory = stack.pop()
            try:
                with os.scandir(directory) as entries:
                    for entry in entries:
                        if entry.is_symlink():
                            continue
                        if entry.is_dir(follow_symlinks=False):
                            stack.append(Path(entry.path))
                        elif entry.is_file(follow_symlinks=False):
                            info = entry.stat(follow_symlinks=False)
                            files[entry.path] = (info.st_size, info.st_mtime_ns)
            except OSError:
                continue
    return files


class Plugin:
    async def _main(self):
        self._task = asyncio.create_task(self._watch())
        self._status = {
            "running": False,
            "game": None,
            "save_roots": [],
            "last_update": None,
            "last_game": None,
            "watched_files": 0,
        }
        decky.logger.info("Checkpoint watcher started")

    async def _unload(self):
        self._task.cancel()
        try:
            await self._task
        except asyncio.CancelledError:
            pass

    async def get_status(self):
        return dict(self._status)

    async def _watch(self):
        previous_files = None
        previous_session = None
        pending = None
        try:
            while True:
                processes = _read_processes()
                session = processes[0] if len(processes) == 1 else None
                roots = _roots()
                files = await asyncio.to_thread(_snapshot, roots)
                self._status.update(
                    running=bool(processes),
                    game=session[1] if session else ("Multiple games" if processes else None),
                    save_roots=[str(root) for root in roots],
                    watched_files=len(files),
                )
                if session != previous_session:
                    # New session: ignore saves that were already present.
                    previous_files = files
                    previous_session = session
                    pending = None
                elif session is not None and previous_files is not None:
                    changed = [
                        path for path, metadata in files.items()
                        if previous_files.get(path) != metadata
                    ]
                    if changed:
                        # Debounce until writes stop.
                        existing_count = pending[1] if pending else 0
                        pending = (time.monotonic(), max(len(changed), existing_count))
                    elif pending and time.monotonic() - pending[0] >= SETTLE_SECONDS:
                        timestamp = datetime.datetime.now().astimezone().isoformat(timespec="seconds")
                        self._status["last_update"] = timestamp
                        self._status["last_game"] = session[1]
                        await decky.emit("save_updated", {
                            "game": session[1],
                            "changed_count": pending[1],
                            "when": timestamp,
                        })
                        pending = None
                    previous_files = files
                else:
                    previous_files = files
                    pending = None
                await asyncio.sleep(SCAN_INTERVAL)
        except asyncio.CancelledError:
            return
        except Exception:
            decky.logger.exception("Checkpoint watcher failed")
