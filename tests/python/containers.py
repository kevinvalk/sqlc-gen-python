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
def container(server: str, variable: str, port: int, arguments: list[str]) -> Iterator[int]:
    """Run a throwaway container of ``arguments`` (options, then the image and its own) and yield the host port its
    ``port`` is published on. We remove it with its volumes afterwards.
    """
    # We exit without Docker instead of skipping, as a proof that silently runs nothing proves nothing.
    docker = shutil.which("docker")
    if docker is None or subprocess.run([docker, "info"], capture_output=True, check=False).returncode != 0:
        sys.exit(f"a {server} server is needed: start docker, or set {variable}")

    name = f"sqlcdx-{secrets.token_hex(4)}"
    run = ["run", "--detach", "--rm", f"--publish=127.0.0.1::{port}", f"--name={name}"]
    subprocess.run([docker, *run, *arguments], check=True, capture_output=True)
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
