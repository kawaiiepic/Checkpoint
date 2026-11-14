"""Checkpoint Decky interface; all detection and save discovery run in Rust."""
import asyncio
import json
import os
import re
from collections import deque
from datetime import datetime
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
        self.events = deque(maxlen=100)
        self.event_id = 0
        self.last_sync = None
        self.last_scan = None
        self._scan_lock = asyncio.Lock()
        self._record("info", "Checkpoint monitor started")
        self._task = asyncio.create_task(self._poll())
        decky.logger.info("Checkpoint Rust status bridge started")

    async def _unload(self):
        self._task.cancel()
        try:
            await self._task
        except asyncio.CancelledError:
            pass

    def _record(self, level, message):
        self.event_id += 1
        self.events.append({
            "id": self.event_id,
            "time": datetime.now().astimezone().isoformat(timespec="seconds"),
            "level": level,
            "message": message[:400],
        })
        if level == "error":
            decky.logger.warning("Checkpoint activity: %s", message)
        else:
            decky.logger.info("Checkpoint activity: %s", message)

    async def get_status(self):
        return {**self.status, "error": self.error, "activity": list(self.events),
                "last_sync": self.last_sync, "last_scan": self.last_scan}

    async def _read_monitor(self):
        """Run the real Rust detector; never merely replay cached status."""
        async with self._scan_lock:
            binary = next((p for p in BINARIES if p.is_file() and os.access(p, os.X_OK)), None)
            if binary is None:
                raise FileNotFoundError("Build and install bin/checkpoint-companion")
            proc = await asyncio.create_subprocess_exec(
                str(binary), "--status-json", env=RUST_ENV,
                stdout=asyncio.subprocess.PIPE, stderr=asyncio.subprocess.PIPE
            )
            try:
                out, err = await asyncio.wait_for(proc.communicate(), timeout=12)
            except asyncio.TimeoutError:
                proc.kill()
                await proc.communicate()
                raise RuntimeError("Rust status scan timed out")
            if proc.returncode:
                raise RuntimeError(err.decode(errors="replace").strip() or f"Rust exited {proc.returncode}")
            result = json.loads(out)
            self.last_scan = datetime.now().astimezone().isoformat(timespec="seconds")
            return result

    async def refresh_detection(self):
        self._record("info", "Manual detection scan requested")
        try:
            current = await self._read_monitor()
            self.status = current
            self.error = None
            self._record("info", "Rust scan: {} Eglantine processes, {} local games, {} save directories, title={}, source={}".format(
                current.get("process_count", 0), current.get("available_titles", 0),
                current.get("available_saves", 0), current.get("title_id") or "none",
                current.get("detection_source") or "none"
            ))
            self._record("info", "Save roots: " + (", ".join(current.get("save_roots") or []) or "none"))
        except Exception as exc:
            self.error = str(exc)
            self._record("error", f"Manual detection failed: {exc}")
        return await self.get_status()

    async def clear_activity(self):
        self.events.clear()
        self._record("info", "Console cleared")
        return True

    async def _poll(self):
        last_session = None
        last_signatures = None
        backup_counter = 0
        last_error = None
        last_inventory = None
        last_configured = None
        sync_was_blocked = False
        tick = 0
        while True:
            try:
                current = await self._read_monitor()
                binary = next(path for path in BINARIES if path.is_file() and os.access(path, os.X_OK))
                tick += 1
                if tick % 12 == 0:
                    self._record("info", "Monitor alive: {} Eglantine processes, {} local games, {} save directories".format(
                        current.get("process_count", 0), current.get("available_titles", 0),
                        current.get("available_saves", 0)))
                session = (bool(current.get("running")), current.get("title_id"))
                signatures = {save["path"]: save["signature"] for save in current.get("saves", [])}
                if last_session is None:
                    self._record("info", "Eglantine running" if session[0] else "Eglantine closed; safe to check sync")
                elif session[0] and not last_session[0]:
                    self._record("game", "Eglantine process launched")
                elif not session[0] and last_session[0]:
                    self._record("game", f"Eglantine exited (last detected title: {last_session[1] or 'unknown'}); sync eligible")
                if session[0] and session[1] and (last_session is None or session[1] != last_session[1] or not last_session[0]):
                    source = current.get("detection_source") or "unknown source"
                    self._record("game", f"Game opened/detected: {current.get('game') or 'Unknown'} [{session[1]}] (via {source})")
                    await decky.emit("game_detected", {
                        "game": current.get("game"), "title_id": session[1]
                    })
                elif session[0] and last_session is not None and session[1] is None and last_session[1] is not None:
                    self._record("info", "Previously identified game is no longer detectable; Eglantine still open, sync paused")
                if last_session == session and last_signatures is not None and signatures != last_signatures and session[1]:
                    changed = sum(last_signatures.get(path) != value for path, value in signatures.items())
                    self._record("save", f"Local save changed for {session[1]} ({changed} file groups); not yet synced")
                    await decky.emit("save_updated", {
                        "game": current.get("game"), "title_id": session[1], "changed_count": changed
                    })
                last_signatures = signatures
                inventory = (current.get("available_titles", 0), current.get("available_saves", 0))
                if inventory != last_inventory:
                    self._record("info", f"Local inventory: {inventory[0]} games, {inventory[1]} save directories")
                    last_inventory = inventory
                configured = bool(current.get("backup_configured"))
                if configured != last_configured:
                    self._record("info", "Cloud destination configured" if configured else "Cloud destination not configured")
                    last_configured = configured
                issue = current.get("backup_error")
                if issue and issue != last_error:
                    self._record("error", f"Cloud configuration: {issue}")
                    last_error = issue
                elif not issue:
                    last_error = None
                self.status = current
                self.error = None
                if current.get("game_running", current.get("running", True)):
                    if not sync_was_blocked:
                        self._record("info", "Cloud sync paused while Eglantine is running")
                    sync_was_blocked = True
                elif sync_was_blocked:
                    backup_counter = 5
                    self._record("sync", "Eglantine closed; starting immediate sync check")
                    sync_was_blocked = False
                # Every ~30 seconds while idle. Rust checks again immediately before copying.
                if not current.get("game_running", current.get("running", True)):
                    backup_counter += 1
                    if backup_counter >= 6:
                        backup_counter = 0
                        if not current.get("backup_configured"):
                            self._record("error", "Sync skipped: no cloud destination configured on this Deck. Run checkpoint-companion --configure-webdav to configure it.")
                            last_session = session
                            await asyncio.sleep(POLL_SECONDS)
                            continue
                        backup = await asyncio.create_subprocess_exec(
                            str(binary), "--backup-once",
                            env=RUST_ENV,
                            stdout=asyncio.subprocess.PIPE, stderr=asyncio.subprocess.PIPE,
                        )
                        self._record("sync", "Checking Nextcloud for save changes")
                        try:
                            backup_out, backup_err = await asyncio.wait_for(backup.communicate(), timeout=90)
                        except asyncio.TimeoutError:
                            backup.kill()
                            await backup.communicate()
                            raise RuntimeError("Automatic sync timed out")
                        stdout = backup_out.decode(errors="replace")
                        stderr = backup_err.decode(errors="replace")
                        # Show only actual Rust sync results, not invented success messages.
                        for line in (stdout + "\n" + stderr).splitlines():
                            if not line.startswith("[Checkpoint]"):
                                continue
                            if any(word in line for word in (
                                "Published current cloud save", "Installed missing save from cloud",
                                "Synced ", "CONFLICT ", "Sync error for ",
                                "Cloud check failed for ", "Sync:", "Sync completed:",
                            )):
                                level = "error" if any(word in line for word in ("CONFLICT", "Sync error", "Cloud check failed")) else "sync"
                                self._record(level, line.removeprefix("[Checkpoint] ").strip())
                        totals = re.search(r"Sync: (\d+) uploads, (\d+) downloads, (\d+) conflicts, (\d+) errors", stdout)
                        if totals:
                            uploads, downloads, conflicts, errors = (int(n) for n in totals.groups())
                            if uploads or downloads:
                                await decky.emit("sync_notification", {
                                    "kind": "success", "title": "Checkpoint cloud sync",
                                    "body": f"{uploads} save(s) uploaded, {downloads} downloaded",
                                })
                            if conflicts or errors:
                                await decky.emit("sync_notification", {
                                    "kind": "error", "title": "Checkpoint needs attention",
                                    "body": f"{conflicts} conflict(s), {errors} error(s). See activity console.",
                                })
                        self.last_sync = datetime.now().astimezone().isoformat(timespec="seconds")
                        if backup.returncode:
                            reason = next((line.removeprefix("[Checkpoint] ").strip()
                                           for line in stderr.splitlines() if line.strip()), "No error details returned")
                            self._record("error", f"Automatic sync failed (exit code {backup.returncode}): {reason}")
                            decky.logger.warning("Checkpoint backup: %s", stderr)
                            if not totals or (int(totals.group(3)) + int(totals.group(4)) == 0):
                                await decky.emit("sync_notification", {
                                    "kind": "error", "title": "Checkpoint sync failed",
                                    "body": reason[:180],
                                })
                        elif "Sync: 0 uploads, 0 downloads, 0 conflicts, 0 errors" in stdout:
                            if (current.get("available_saves") or 0) > 0 and (current.get("backed_up_games") or 0) == 0:
                                self._record("info", "Sync checked: no transfers. Local saves exist but no paired games. Confirm matching NUP is installed and listed in Eglantine game directories.")
                            else:
                                self._record("sync", "Sync complete: no changes to upload or download")
                        elif not any(line.startswith("[Checkpoint] Sync:") for line in stdout.splitlines()):
                            self._record("info", "Sync command completed without a native WebDAV transfer summary; check configured backend")
                else:
                    backup_counter = 0
                last_session = session
            except asyncio.CancelledError:
                raise
            except Exception as exc:
                self.error = str(exc)
                decky.logger.warning("Checkpoint Rust bridge: %s", exc)
                if str(exc) != last_error:
                    self._record("error", f"Monitor error: {exc}")
                    last_error = str(exc)
                last_session = None
                last_signatures = None
            await asyncio.sleep(POLL_SECONDS)
