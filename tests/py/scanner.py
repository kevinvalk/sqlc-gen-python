"""Prove the SQL rewriting for SQLAlchemy's ``text()``
(``src/backend/sqlalchemy/text.rs``) against PostgreSQL's own scanner.

usage: scanner.py

Hypothesis draws SQL from a grammar of what PostgreSQL scans specially:
standard, ``E``, ``U&`` and bit strings, with ``''``, backslashes and
continuations across a newline; dollar quotes and their tags; nested
``/* */`` and ``--`` comments ending at ``\\n``, ``\\r`` or the end; quoted
identifiers; casts, colons and ``:name``-like text; identifiers holding
``$``; and placeholders glued to all of them. For every text PostgreSQL scans
(pglast is libpg_query, the scanner sqlc parses with), ``text`` (through
``cargo run --example sql_text``) rewrites it for exactly the placeholders
PostgreSQL found, and SQLAlchemy's ``text()`` must bind exactly those, at
exactly their positions, and compile back to the input byte for byte: as
SQLAlchemy renders it, and as its asyncpg dialect sends it, each bind a
``$N`` of its own. The rewriting may refuse only what ``text()`` cannot
represent (``docs/python-output.md`` §6, rule 3), and a named parameter sqlc
left as written (rule 2). No database is needed: ``cargo xtask check`` runs it.
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
# A placeholder's parameter name: no fragment below spells it, so in what
# SQLAlchemy compiles, every match is a bind.
NAME = "zq{}"
BIND = re.compile(r":zq(\d+)")
# sqlc's functions, as the tokens of a call, which sqlc rewrites wherever it
# analyses a statement.
SQLC_FUNCTIONS = {f"sqlc . {name}" for name in ("arg", "narg", "slice", "embed")}


# A letter of Unicode 14 (Python 3.11+) and a code point not yet assigned:
# `\w` on some supported Python, not on Python 3.10. Every colon escaped, the
# rewriting reads the same on all of them, which this layer proves on each.
UNSETTLED = ["\u0870", "\u0378"]

# The grammar. Inside a quote or a comment, anything another fragment would
# end, start or bind.
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
# Between two strings: whitespace holding a newline continues the first into
# the second; a space does not, nor, to libpg_query, a comment.
NEWLINE = st.sampled_from(["\n", " \r\n", "\r", " -- c\n", "\n-- c\n\t", "\v\n\f", " "])


def body(pieces: list[str]) -> st.SearchStrategy[str]:
    """Some of ``pieces``, joined."""
    return st.lists(st.sampled_from(pieces), max_size=6).map("".join)


def quoted(quote: str, prefixes: list[str]) -> st.SearchStrategy[str]:
    """A ``quote``-delimited token, its quotes in it doubled."""
    return st.builds(
        lambda prefix, text: prefix + quote + text.replace(quote, 2 * quote) + quote,
        st.sampled_from(prefixes),
        body(BODY),
    )


# The inside of an E string, where a quote may also be `\'`.
E_BODY = body([*BODY, "''", "\\'", "\\\\", "\\n", "\\x41", "\\u00e9"]).map(
    lambda text: re.sub(r"(?<!\\)'(?!')", "''", text)
)
E_STRING = st.builds(lambda text: f"E'{text}'", E_BODY)
STRING = quoted("'", ["", "N", "U&", "B", "X", "e"]) | E_STRING
CONTINUED = st.builds(
    lambda first, newline, rest: f"{first}{newline}{rest}",
    STRING,
    NEWLINE,
    # A next string that ends elsewhere if it continues an E string.
    E_BODY.map(lambda text: f"'{text}'")
    | st.sampled_from(["'\\' $1 '", "'\\' $1 ''", "'\\\\' $1 '", "'\\'' $1 '"]),
)
DOLLAR = st.builds(
    lambda tag, text: f"${tag}${text}${tag}$",
    TAG,
    body([*BODY, "$ab$", "$b$"]),
)
IDENTIFIER = quoted('"', ["", "U&"])
LINE_COMMENT = st.builds(
    lambda text, end: f"--{text}{end}",
    body([p for p in BODY if p not in "\n\r"]),
    st.sampled_from(["\n", "\r", "\r\n", ""]),
)
# Nested, with text between the two ends.
BLOCK_COMMENT = st.builds(
    lambda before, inner, after: f"/*{before}/*{inner}*/{after}*/",
    body(BODY),
    body([p for p in BODY if p not in {"/*", "*/"}]),
    body([p for p in BODY if p not in {"/*", "*/"}]),
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
TEXT = st.lists(FRAGMENT, min_size=1, max_size=16).map(
    lambda fragments: "SELECT " + "".join(fragments)
)


@dataclass(frozen=True)
class Placeholder:
    """A ``PARAM`` token of PostgreSQL's scanner."""

    start: int
    text: str

    @property
    def number(self) -> int:
        """The parameter it binds."""
        return int(self.text[1:])


def placeholders(sql: str) -> list[Placeholder] | None:
    """The placeholders PostgreSQL scans in ``sql``, or ``None`` if it cannot
    scan it, so that sqlc would not have parsed it.
    """
    try:
        tokens = parser.scan(sql)
    except parser.ParseError:
        return None
    # Offsets count characters, and `end` is the token's last one.
    return [
        Placeholder(t.start, sql[t.start : t.end + 1])
        for t in tokens
        if t.name == "PARAM"
    ]


class Rewriter:
    """SQLAlchemy's ``text`` rewriting in a process of its own."""

    def __init__(self) -> None:
        cargo = os.environ.get("CARGO", "cargo")
        self.process = subprocess.Popen(
            [cargo, "run", "--quiet", "--example", "sql_text"],
            cwd=ROOT,
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            text=True,
            encoding="utf-8",
        )
        # Both are pipes.
        self.stdin = cast("IO[str]", self.process.stdin)
        self.stdout = cast("IO[str]", self.process.stdout)

    def __call__(self, sql: str, numbers: set[int]) -> dict[str, str]:
        """``sql`` rewritten for ``numbers``: ``{"Ok": text}`` or ``{"Err": why}``."""
        params = [[n, NAME.format(n)] for n in sorted(numbers)]
        self.stdin.write(json.dumps([sql, params]) + "\n")
        self.stdin.flush()
        if not (line := self.stdout.readline()):
            message = "sql_text exited"
            raise RuntimeError(message)
        result: dict[str, str] = json.loads(line)
        return result

    def close(self) -> None:
        """Let the process finish."""
        self.stdin.close()
        self.process.wait()


def refusal(sql: str, found: list[Placeholder]) -> str | None:
    """Why the rewriting must refuse ``sql``, if it must.

    In the order the rewriting checks: sqlc left a named parameter as written,
    as it does in a
    statement it does not analyse (a lone ``@`` operator directly before what
    starts an identifier, or ``sqlc.arg`` and its kin); or a placeholder
    directly follows ``:``, ``\\`` or a word character (a digit of another
    placeholder, say), or runs into a word character, which ``text()`` would
    read as part of its bind.
    """
    code = [t for t in parser.scan(sql) if t.name not in {"C_COMMENT", "SQL_COMMENT"}]
    texts = [sql[t.start : t.end + 1] for t in code]
    for i, token in enumerate(code):
        follows = sql[token.end + 1 : token.end + 2]
        at = (texts[i], token.name) == ("@", "Op")
        if at and (follows == '"' or identifier(follows)):
            return "sqlc did not rewrite"
        if " ".join(texts[i : i + 3]).lower() in SQLC_FUNCTIONS:
            return "sqlc did not rewrite"
    before = (sql[p.start - 1] for p in found)
    after = (sql[p.start + len(p.text) :][:1] for p in found)
    if any(c in {":", "\\"} or word(c) for c in before) or any(word(c) for c in after):
        return "SQLAlchemy text() cannot"
    return None


def identifier(c: str) -> bool:
    """Whether PostgreSQL starts an identifier with ``c`` (``ident_start``)."""
    return c == "_" or (c.isascii() and c.isalpha()) or not c.isascii()


def word(c: str) -> bool:
    """Whether ``c`` may be a character of ``\\w``, as SQLAlchemy matches
    binds, on any supported Python: an ASCII one, or any outside ASCII.
    """
    return not c.isascii() or re.fullmatch(r"\w", c) is not None


def check(sql: str, rewrite: Rewriter, tally: Counter[str]) -> None:
    """Hold the rewriting of ``sql`` to PostgreSQL's scanner and SQLAlchemy."""
    found = placeholders(sql)
    if found is None:
        tally["not scanned by PostgreSQL"] += 1
        return
    result = rewrite(sql, {p.number for p in found})
    reason = refusal(sql, found)
    if error := result.get("Err"):
        # For that reason, not some other, such as a placeholder it did not find.
        expect(reason is not None and reason in error, f"refused {sql!r}: {error}")
        tally[f"refused: {reason}"] += 1
        return
    rewritten = result["Ok"]
    expect(reason is None, f"did not refuse {sql!r}: {rewritten!r}")
    compiled = str(sqlalchemy.text(rewritten).compile())
    # Each bind back as the placeholder PostgreSQL found, noting where it lands.
    pieces = BIND.split(compiled)
    numbers = [int(n) for n in pieces[1::2]]
    expect(
        numbers == [p.number for p in found],
        f"{sql!r} rewritten {rewritten!r} binds {numbers}",
    )
    undone, starts = pieces[0], list[int]()
    for placeholder, text in zip(found, pieces[2::2], strict=True):
        starts.append(len(undone))
        undone += placeholder.text + text
    expect(
        (undone, starts) == (sql, [p.start for p in found]),
        f"{sql!r} rewritten {rewritten!r} compiles to {compiled!r}",
    )
    # asyncpg is sent each bind as `$k`, numbered by its first appearance, and
    # the values in that order.
    on_asyncpg = sqlalchemy.text(rewritten).compile(dialect=ASYNCPG)
    order = list(dict.fromkeys(numbers))
    sent = pieces[0] + "".join(
        f"${order.index(n) + 1}{text}"
        for n, text in zip(numbers, pieces[2::2], strict=True)
    )
    expect(
        (str(on_asyncpg), on_asyncpg.positiontup)
        == (sent, [NAME.format(n) for n in order]),
        f"{sql!r} rewritten {rewritten!r} compiles on asyncpg to {on_asyncpg!r}",
    )
    tally["scanned and rewritten"] += 1


def expect(holds: bool, discrepancy: str) -> None:  # noqa: FBT001
    """Fail the example unless ``holds``."""
    if not holds:
        raise AssertionError(discrepancy)


def prove() -> str:
    """Run the property; raise on a discrepancy, shrunk, or if too few
    examples were held to anything. Return a line of tallies.
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
    return ", ".join(f"{n} {what}" for what, n in sorted(scanned.items()))


def main() -> int:
    """Prove the rewriting, raising on a discrepancy."""
    print(prove())
    return 0


if __name__ == "__main__":
    sys.exit(main())
