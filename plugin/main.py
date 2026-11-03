"""Decky backend: a thin bridge between the panel and `steam-maintenance plugin`.

The plugin runs as root (`"flags": ["root"]`). Verbs that read or write the Steam
user's state run as that user; the ones that drive the system unit run as root.
`watch` streams the check's prompts and the worker's progress as plugin events.
"""

import asyncio
import json
import os
import pwd

import decky

EXE = "@steamMaintenance@"
USER_VERBS = {"state", "probe", "later", "snooze", "heartbeat", "watch"}
WATCH_RESTART_DELAY = 5


def _as_user():
    """Popen keywords that drop to the Steam user with the environment the user verbs expect."""
    pw = pwd.getpwnam(decky.DECKY_USER)
    return {
        "user": pw.pw_uid,
        "group": pw.pw_gid,
        "extra_groups": [],
        "env": {
            "HOME": pw.pw_dir,
            "USER": pw.pw_name,
            "XDG_RUNTIME_DIR": f"/run/user/{pw.pw_uid}",
            "PATH": os.environ.get("PATH", "/run/current-system/sw/bin"),
        },
    }


class Plugin:
    def __init__(self):
        self._watch_task = None
        self._watch_proc = None

    async def _run(self, verb, *args):
        kwargs = _as_user() if verb in USER_VERBS else {}
        proc = await asyncio.create_subprocess_exec(
            EXE,
            "plugin",
            verb,
            *args,
            stdin=asyncio.subprocess.DEVNULL,
            stdout=asyncio.subprocess.PIPE,
            stderr=asyncio.subprocess.PIPE,
            **kwargs,
        )
        out, err = await proc.communicate()
        if proc.returncode != 0:
            message = err.decode(errors="replace").strip() or f"{verb} exited with {proc.returncode}"
            decky.logger.error("%s: %s", verb, message)
            raise RuntimeError(message)
        return json.loads(out)

    # ---- callable from the frontend -------------------------------------

    async def state(self):
        return await self._run("state")

    async def probe(self):
        return await self._run("probe")

    async def later(self):
        return await self._run("later")

    async def snooze(self):
        return await self._run("snooze")

    async def heartbeat(self):
        return await self._run("heartbeat")

    async def start(self, rev=None):
        return await self._run("start", *([rev] if rev else []))

    async def stop(self):
        return await self._run("stop")

    async def reboot(self):
        return await self._run("reboot")

    # ---- lifecycle --------------------------------------------------------

    async def _main(self):
        decky.logger.info("starting the watch")
        self._watch_task = asyncio.get_running_loop().create_task(self._watch())

    async def _unload(self):
        if self._watch_task:
            self._watch_task.cancel()
        if self._watch_proc and self._watch_proc.returncode is None:
            self._watch_proc.terminate()

    async def _watch(self):
        while True:
            try:
                self._watch_proc = await asyncio.create_subprocess_exec(
                    EXE,
                    "plugin",
                    "watch",
                    stdin=asyncio.subprocess.DEVNULL,
                    stdout=asyncio.subprocess.PIPE,
                    stderr=asyncio.subprocess.STDOUT,
                    **_as_user(),
                )
                async for line in self._watch_proc.stdout:
                    try:
                        event = json.loads(line)
                    except ValueError:
                        decky.logger.warning("watch: %s", line.decode(errors="replace").rstrip())
                        continue
                    await decky.emit(event["event"], event)
                await self._watch_proc.wait()
                decky.logger.warning("watch exited with %s", self._watch_proc.returncode)
            except asyncio.CancelledError:
                raise
            except Exception as err:  # noqa: BLE001 - keep the watch alive whatever happened
                decky.logger.error("watch: %s", err)
            await asyncio.sleep(WATCH_RESTART_DELAY)
