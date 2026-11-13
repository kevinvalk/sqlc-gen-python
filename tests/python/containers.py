"""Throwaway Docker containers, for the database servers the proof runs on."""

from __future__ import annotations

import secrets
import shutil
import subprocess
import sys
from contextlib import contextmanager
from typing import TYPE_CHECKING

if TYPE_CHECKING:
    from collections.abc import Iterator


@contextmanager
def container(what: str, env: str, port: int, args: list[str]) -> Iterator[int]:
    """Run a container of ``args`` (options, then the image and its own),
    yielding the host port its ``port`` is published on; removed with its
    volumes afterwards. Without Docker, exit: a ``what`` server is needed, or
    the variable ``env`` naming one. There is no silent skip.
    """
    docker = shutil.which("docker")
    if (
        docker is None
        or subprocess.run([docker, "info"], capture_output=True, check=False).returncode
    ):
        sys.exit(f"a {what} server is needed: start docker, or set {env}")
    name = f"sqlcdx-{secrets.token_hex(4)}"
    run = ["run", "--detach", "--rm", f"--publish=127.0.0.1::{port}", f"--name={name}"]
    subprocess.run([docker, *run, *args], check=True, capture_output=True)
    try:
        published = subprocess.run(
            [docker, "port", name, f"{port}/tcp"],
            check=True,
            capture_output=True,
            text=True,
        ).stdout
        yield int(published.splitlines()[0].rsplit(":", 1)[1])
    finally:
        subprocess.run(
            [docker, "rm", "--force", "--volumes", name],
            capture_output=True,
            check=False,
        )
