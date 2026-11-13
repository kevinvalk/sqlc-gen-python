"""What `cargo test` cannot check without Python.

`cargo test` holds every golden case and corpus output to sqlc's request
(`tests/output/support/mod.rs`): names, signatures, result shapes, and SQL that
round-trips byte for byte through copies of SQLAlchemy's own expressions.
This suite pins those copies to the SQLAlchemy it runs.
"""

import re
import sys
import unicodedata

from sqlalchemy.sql.compiler import BIND_PARAMS_ESC
from sqlalchemy.sql.elements import TextClause

from conftest import ROOT


def test_the_sql_oracle_is_sqlalchemys_own() -> None:
    """`cargo test` round-trips every query's SQL through copies of
    SQLAlchemy's bind and escape expressions: they must be this SQLAlchemy's.
    """
    copied = (ROOT / "tests/output/support/sqlalchemy.re").read_text().splitlines()
    bind = TextClause._bind_params_regex  # noqa: SLF001 -- SQLAlchemy's own definition of a bind
    assert copied == [bind.pattern, BIND_PARAMS_ESC.pattern]


def test_a_word_is_a_letter_a_number_or_an_underscore() -> None:
    """`cargo test` reads `\\w` in SQLAlchemy's expressions as Python's `re`
    matches it: `[\\p{L}\\p{N}_]`, by the
    general categories of this Python's Unicode database.
    """
    word = re.compile(r"\w")
    differ = [
        f"U+{code:04X}"
        for code in range(sys.maxunicode + 1)
        if not 0xD800 <= code <= 0xDFFF  # surrogates are no characters
        and bool(word.fullmatch(c := chr(code)))
        != (c == "_" or unicodedata.category(c)[0] in "LN")
    ]
    assert not differ, differ[:10]
