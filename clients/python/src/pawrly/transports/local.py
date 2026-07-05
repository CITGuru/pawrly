"""In-process transport: spawns a `pawrly console` child on a private loopback
port and talks to it over REST — an engine this client owns and tears down."""

import os
import shutil
import socket
import subprocess
import tempfile
import time

from ..errors import PawrlyError
from .rest import RestTransport


def _free_port() -> int:
    s = socket.socket()
    s.bind(("127.0.0.1", 0))
    port = s.getsockname()[1]
    s.close()
    return port


class LocalTransport(RestTransport):
    """A `pawrly console` the client spawns, owns, and tears down on `close()`."""

    name = "local"

    def __init__(
        self,
        config: str | None = None,
        home: str | None = None,
        binary: str = "pawrly",
    ) -> None:
        self._tmp = tempfile.mkdtemp(prefix="pawrly-local-")
        if config is None:
            config = os.path.join(self._tmp, "pawrly.yaml")
            with open(config, "w") as f:
                f.write("version: 1\n")

        port = _free_port()
        args = [binary]
        if home:
            args += ["--home", str(home)]
        args += ["--config", str(config), "console", "--addr", f"127.0.0.1:{port}"]
        self._proc = subprocess.Popen(
            args, stdout=subprocess.DEVNULL, stderr=subprocess.PIPE
        )
        super().__init__(f"http://127.0.0.1:{port}")

        deadline = time.time() + 10
        while True:
            if self._proc.poll() is not None:
                err = self._proc.stderr.read().decode() if self._proc.stderr else ""
                self._cleanup()
                raise PawrlyError("PAWRLY_INTERNAL", f"pawrly console exited: {err}")
            try:
                if self.health().ok:
                    break
            except Exception:
                pass  # not up yet
            if time.time() > deadline:
                self._terminate()
                self._cleanup()
                raise PawrlyError(
                    "PAWRLY_INTERNAL", "pawrly console never became healthy"
                )
            time.sleep(0.05)

    def _terminate(self) -> None:
        if self._proc.poll() is None:
            self._proc.terminate()
            try:
                self._proc.wait(timeout=5)
            except subprocess.TimeoutExpired:
                self._proc.kill()

    def _cleanup(self) -> None:
        shutil.rmtree(self._tmp, ignore_errors=True)

    def close(self) -> None:
        super().close()
        self._terminate()
        self._cleanup()

    def __del__(self) -> None:
        try:
            self._terminate()
        except Exception:
            pass
