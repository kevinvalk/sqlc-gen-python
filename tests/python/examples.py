"""The golden cases: each ``codegen`` entry of each ``examples/*/sqlc.yaml``.

Beside an example's ``sqlc.yaml`` and SQL sits one directory per codegen ``out``, named for the case, holding what the
plugin generates. What sqlc sends the plugin for each example is captured in ``tests/sqlc/examples/<example>.json``
(``cargo xtask capture``).
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
    name: str
    example: str
    options: dict[str, Any]

    @property
    def dir(self) -> Path:
        return EXAMPLES / self.example / self.name

    @property
    def request(self) -> Path:
        return REQUESTS / f"{self.example}.json"

    @property
    def backend(self) -> str:
        return str(self.options["backend"])

    @property
    def is_async(self) -> bool:
        return bool(self.options["async"])

    @property
    def models(self) -> str:
        """The module of the case's models: its own, or the one ``models.module`` names."""
        return str(self.options.get("models", {}).get("module", f"{self.name}.models"))

    @property
    def is_models_only(self) -> bool:
        return self.options.get("models", {}).get("only") is True

    @property
    def schema_sql(self) -> Path:
        return EXAMPLES / self.example / "schema.sql"


@cache
def golden_cases() -> tuple[GoldenCase, ...]:
    cases: list[GoldenCase] = []
    for configuration_path in sorted(EXAMPLES.glob("*/sqlc.yaml")):
        configuration: dict[str, Any] = yaml.safe_load(configuration_path.read_text())
        cases.extend(
            GoldenCase(codegen["out"], configuration_path.parent.name, codegen.get("options", {}))
            for package in configuration["sql"]
            for codegen in package.get("codegen", [])
        )
    return tuple(cases)


def statements(script: str) -> list[str]:
    """Split a schema script into statements. We only handle what our schemas hold: comments are whole ``--`` lines, and
    there is no ``;`` other than between statements.
    """
    code = "\n".join(line for line in script.splitlines() if not line.lstrip().startswith("--"))
    return [statement for statement in code.split(";") if statement.strip() != ""]
