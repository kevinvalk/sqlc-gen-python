"""The golden cases: each ``codegen`` entry of each ``examples/*/sqlc.yaml``.

An example is a sqlc project: its ``sqlc.yaml``, its SQL, and beside them one
directory per codegen ``out``, named for the case, holding what the plugin
generates for it. What sqlc sends the plugin for each example is captured in
``tests/sqlc/examples/<example>.json`` (``cargo xtask capture``).
"""

from __future__ import annotations

from dataclasses import dataclass
from functools import cache
from pathlib import Path
from typing import Any

import yaml

ROOT = Path(__file__).resolve().parents[2]
EXAMPLES = ROOT / "examples"
REQUESTS = ROOT / "tests/sqlc/examples"


@dataclass(frozen=True)
class GoldenCase:
    """One codegen entry of an example's ``sqlc.yaml``."""

    name: str
    example: str
    options: dict[str, Any]

    @property
    def dir(self) -> Path:
        """The committed directory of what it generates."""
        return EXAMPLES / self.example / self.name

    @property
    def request(self) -> Path:
        """What sqlc sends the plugin for its example, captured."""
        return REQUESTS / f"{self.example}.json"

    @property
    def backend(self) -> str:
        """The library it runs its SQL through."""
        return str(self.options["backend"])

    @property
    def is_async(self) -> bool:
        """Whether its functions are coroutines."""
        return bool(self.options["async"])

    @property
    def schema_sql(self) -> Path:
        """Its example's schema."""
        return EXAMPLES / self.example / "schema.sql"


@cache
def golden_cases() -> tuple[GoldenCase, ...]:
    """Every golden case, by example, in ``sqlc.yaml`` order."""
    found: list[GoldenCase] = []
    for config in sorted(EXAMPLES.glob("*/sqlc.yaml")):
        parsed: dict[str, Any] = yaml.safe_load(config.read_text())
        found.extend(
            GoldenCase(codegen["out"], config.parent.name, codegen.get("options", {}))
            for package in parsed["sql"]
            for codegen in package.get("codegen", [])
        )
    return tuple(found)
