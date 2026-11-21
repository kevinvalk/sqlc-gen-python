"""Prove the SQL rewriting for SQLAlchemy's ``text()`` (``src/backend/sqlalchemy/text.rs``) against PostgreSQL's own
scanner, as ``docs/testing.md`` describes. No database is needed, so ``cargo xtask check`` runs it.

usage: scanner.py
"""

from __future__ import annotations

import json
import os
import re
import subprocess
import sys
from collections import Counter
from dataclasses import dataclass
from pathlib import Path
from typing import IO, cast

import sqlalchemy
from hypothesis import HealthCheck, given, settings
from hypothesis import strategies as st
from pglast import parser

ROOT = Path(__file__).resolve().parents[2]
SCANNED = 4000
ASYNCPG = sqlalchemy.URL.create("postgresql+asyncpg").get_dialect()()
# No fragment below spells this parameter name, so every match in what SQLAlchemy compiles is a bind.
NAME = "zq{}"
BIND = re.compile(r":zq(\d+)")
# The tokens of a call to one of sqlc's functions, which sqlc rewrites wherever it analyses a statement.
SQLC_FUNCTIONS = {f"sqlc . {name} (" for name in ("arg", "narg", "slice", "embed")}


# A letter of Unicode 15 (Python 3.12+) and a code point not yet assigned, so `\w` on some supported Python but not on
# 3.11. As every colon is escaped the rewriting reads the same on all of them, and drawing these proves that on the one
# Python we run on.
UNSETTLED = ["\U00011f04", "\u0378"]

# Inside a quote or a comment we draw anything another fragment would end, start or bind.
BODY = [
    *["$1", "$2", "$", "$$", "$a$", "$a", ":", ":x", "::", "\\", "'", '"'],
    *["--", "/*", "*/", "\n", "\r", " ", "a", "é", "Ⓐ", "\u0345", "%", "%(x)s"],
    *UNSETTLED,
]
CODE = [
    *[" ", "\n", "\r", "\r\n", "\t", "\f", "\v", ",", "(", ")", "[", "]", ";", "&"],
    *[":", ":x", "::", "::int", "::text[]", "[1:2]", "a:b", "\\", "\\:", "%"],
    *["a", "E", "e", "U", "b", "x1", "a$", "a$1", "é", "Ⓐ", "_", "\u0345"],
    *["1", "1.5", "0x1F", "1_000", "1e5", "+", "-", "*", "/", "||", "@>"],
    *["@", "@a", "sqlc.arg(", "SQLC . narg"],
    *UNSETTLED,
]
PLACEHOLDER = st.sampled_from(["$1", "$2", "$3", "$01"])
TAG = st.sampled_from(["", "a", "ab", "é", "_1", "A"])
# Whitespace holding a newline between two strings continues the first into the second. A space does not, and to
# libpg_query neither does a comment!
NEWLINE = st.sampled_from(["\n", " \r\n", "\r", " -- c\n", "\n-- c\n\t", "\v\n\f", " "])


def body(pieces: list[str]) -> st.SearchStrategy[str]:
    return st.lists(st.sampled_from(pieces), max_size=6).map("".join)


def quoted(quote: str, prefixes: list[str]) -> st.SearchStrategy[str]:
    return st.builds(
        lambda prefix, text: prefix + quote + text.replace(quote, 2 * quote) + quote,
        st.sampled_from(prefixes),
        body(BODY),
    )


# Inside an E string a quote may also be `\'`.
E_BODY = body([*BODY, "''", "\\'", "\\\\", "\\n", "\\x41", "\\u00e9"]).map(
    lambda text: re.sub(r"(?<!\\)'(?!')", "''", text)
)
E_STRING = st.builds(lambda text: f"E'{text}'", E_BODY)
STRING = quoted("'", ["", "N", "U&", "B", "X", "e"]) | E_STRING
CONTINUED = st.builds(
    lambda first, newline, rest: f"{first}{newline}{rest}",
    STRING,
    NEWLINE,
    # A next string that ends elsewhere when it continues an E string.
    E_BODY.map(lambda text: f"'{text}'") | st.sampled_from(["'\\' $1 '", "'\\' $1 ''", "'\\\\' $1 '", "'\\'' $1 '"]),
)
DOLLAR = st.builds(
    lambda tag, text: f"${tag}${text}${tag}$",
    TAG,
    body([*BODY, "$ab$", "$b$"]),
)
IDENTIFIER = quoted('"', ["", "U&"])
LINE_COMMENT = st.builds(
    lambda text, end: f"--{text}{end}",
    body([piece for piece in BODY if piece not in "\n\r"]),
    st.sampled_from(["\n", "\r", "\r\n", ""]),
)
# Nested, with text between the two ends.
BLOCK_COMMENT = st.builds(
    lambda before, inner, after: f"/*{before}/*{inner}*/{after}*/",
    body(BODY),
    body([piece for piece in BODY if piece not in {"/*", "*/"}]),
    body([piece for piece in BODY if piece not in {"/*", "*/"}]),
)
FRAGMENT = st.one_of(
    st.sampled_from(CODE),
    PLACEHOLDER,
    STRING,
    CONTINUED,
    DOLLAR,
    IDENTIFIER,
    LINE_COMMENT,
    BLOCK_COMMENT,
)
TEXT = st.lists(FRAGMENT, min_size=1, max_size=16).map(lambda fragments: "SELECT " + "".join(fragments))


@dataclass(frozen=True)
class Placeholder:
    start: int
    text: str

    @property
    def number(self) -> int:
        return int(self.text[1:])


def placeholders(sql: str) -> list[Placeholder] | None:
    """The ``PARAM`` tokens PostgreSQL scans in ``sql``, or ``None`` when it cannot scan it, as sqlc would not have
    parsed it either.
    """
    try:
        tokens = parser.scan(sql)
    except parser.ParseError:
        return None

    # NOTE: Offsets count characters, and `end` is the token's last one.
    return [Placeholder(token.start, sql[token.start : token.end + 1]) for token in tokens if token.name == "PARAM"]


class Rewriter:
    """The plugin's rewriting for SQLAlchemy's ``text()``, in a ``cargo xtask sql-text`` process of its own."""

    def __init__(self) -> None:
        cargo = os.environ.get("CARGO", "cargo")
        self.process = subprocess.Popen(
            [cargo, "run", "--quiet", "--package", "xtask", "--", "sql-text"],
            cwd=ROOT,
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            text=True,
            encoding="utf-8",
        )
        # Both are pipes, so never None.
        self.stdin = cast("IO[str]", self.process.stdin)
        self.stdout = cast("IO[str]", self.process.stdout)

    def __call__(self, sql: str, numbers: set[int]) -> dict[str, str]:
        """Rewrite ``sql`` for the parameter ``numbers`` into ``{"Ok": text}`` or ``{"Err": reason}``."""
        parameters = [[number, NAME.format(number)] for number in sorted(numbers)]
        self.stdin.write(json.dumps([sql, parameters]) + "\n")
        self.stdin.flush()
        if (line := self.stdout.readline()) == "":
            message = "cargo xtask sql-text exited"
            raise RuntimeError(message)
        result: dict[str, str] = json.loads(line)
        return result

    def close(self) -> None:
        self.stdin.close()
        self.process.wait()


def refusal(sql: str, found: list[Placeholder]) -> str | None:
    """Why the rewriting must refuse ``sql``, if it must, checked in the order the rewriting checks. It may refuse only
    for these.
    """
    code = [token for token in parser.scan(sql) if token.name not in {"C_COMMENT", "SQL_COMMENT"}]
    texts = [sql[token.start : token.end + 1] for token in code]

    # sqlc leaves a named parameter as written in a statement it does not analyse: a lone `@` operator directly before
    # what starts an identifier, or a call of `sqlc.arg` and its kin.
    for i, token in enumerate(code):
        follows = sql[token.end + 1 : token.end + 2]
        is_at = (texts[i], token.name) == ("@", "Op")
        if is_at and (follows == '"' or identifier(follows)):
            return "sqlc did not rewrite"
        if " ".join(texts[i : i + 4]).lower() in SQLC_FUNCTIONS:
            return "sqlc did not rewrite"

    # `text()` would read a placeholder as part of its bind when it directly follows `:`, `\\` or a word character (a
    # digit of another placeholder, say), or runs into a word character.
    before = (sql[placeholder.start - 1] for placeholder in found)
    after = (sql[placeholder.start + len(placeholder.text) :][:1] for placeholder in found)
    if any(c in {":", "\\"} or word(c) for c in before) or any(word(c) for c in after):
        return "SQLAlchemy text() cannot"

    return None


def identifier(character: str) -> bool:
    """Whether PostgreSQL starts an identifier with ``character`` (``ident_start``)."""
    return character == "_" or (character.isascii() and character.isalpha()) or not character.isascii()


def word(character: str) -> bool:
    """Whether ``character`` may match ``\\w``, as SQLAlchemy matches binds, on any supported Python. We take every
    non-ASCII character as one, as which of them match differs per Python.
    """
    return not character.isascii() or re.fullmatch(r"\w", character) is not None


def check(sql: str, rewrite: Rewriter, tally: Counter[str]) -> None:
    found = placeholders(sql)
    if found is None:
        tally["not scanned by PostgreSQL"] += 1
        return

    result = rewrite(sql, {placeholder.number for placeholder in found})
    reason = refusal(sql, found)
    if (error := result.get("Err")) is not None:
        # It must refuse for that reason and not some other, such as a placeholder it did not find.
        expect(reason is not None and reason in error, f"refused {sql!r}: {error}")
        tally[f"refused: {reason}"] += 1
        return

    rewritten = result["Ok"]
    expect(reason is None, f"did not refuse {sql!r}: {rewritten!r}")
    compiled = str(sqlalchemy.text(rewritten).compile())

    # We put each bind back as the placeholder PostgreSQL found, noting where it lands.
    pieces = BIND.split(compiled)
    numbers = [int(number) for number in pieces[1::2]]
    expect(
        numbers == [placeholder.number for placeholder in found],
        f"{sql!r} rewritten {rewritten!r} binds {numbers}",
    )
    undone, starts = pieces[0], list[int]()
    for placeholder, text in zip(found, pieces[2::2], strict=True):
        starts.append(len(undone))
        undone += placeholder.text + text
    expect(
        (undone, starts) == (sql, [placeholder.start for placeholder in found]),
        f"{sql!r} rewritten {rewritten!r} compiles to {compiled!r}",
    )

    # SQLAlchemy sends asyncpg each bind as `$k`, numbered by its first appearance, and the values in that order.
    on_asyncpg = sqlalchemy.text(rewritten).compile(dialect=ASYNCPG)
    order = list(dict.fromkeys(numbers))
    sent = pieces[0] + "".join(f"${order.index(n) + 1}{text}" for n, text in zip(numbers, pieces[2::2], strict=True))
    expect(
        (str(on_asyncpg), on_asyncpg.positiontup) == (sent, [NAME.format(n) for n in order]),
        f"{sql!r} rewritten {rewritten!r} compiles on asyncpg to {on_asyncpg!r}",
    )
    tally["scanned and rewritten"] += 1


def expect(holds: bool, discrepancy: str) -> None:  # noqa: FBT001
    if not holds:
        raise AssertionError(discrepancy)


def prove() -> str:
    """Run the property and return a line of tallies. We raise on a discrepancy (shrunk), and also when too few examples
    were held to anything, as then the property proves little.
    """
    scanned = Counter[str]()
    rewrite = Rewriter()

    @settings(
        max_examples=SCANNED,
        deadline=None,
        database=None,
        derandomize=True,
        suppress_health_check=[HealthCheck.too_slow],
    )
    @given(TEXT)
    def scans_as_postgresql(sql: str) -> None:
        check(sql, rewrite, scanned)

    try:
        scans_as_postgresql()
    finally:
        rewrite.close()
    expect(scanned["scanned and rewritten"] > SCANNED // 4, f"too few: {scanned}")
    return ", ".join(f"{count} {outcome}" for outcome, count in sorted(scanned.items()))


def main() -> int:
    print(prove())
    return 0


if __name__ == "__main__":
    sys.exit(main())
