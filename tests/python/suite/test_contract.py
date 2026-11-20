"""What `cargo test` cannot check without Python.

`cargo test` round-trips the SQL of every golden case and endtoend output byte for byte through copies of SQLAlchemy's
own expressions (`tests/rust/support/`), and here we pin those copies to the SQLAlchemy we run.
"""

import re
import sys
import unicodedata

from examples import ROOT
from sqlalchemy.sql.compiler import BIND_PARAMS_ESC
from sqlalchemy.sql.elements import TextClause


def test_the_sql_oracle_is_sqlalchemys_own() -> None:
    copied = (ROOT / "tests/rust/support/sqlalchemy.re").read_text().splitlines()
    bind = TextClause._bind_params_regex  # noqa: SLF001 -- SQLAlchemy's own definition of a bind
    assert copied == [bind.pattern, BIND_PARAMS_ESC.pattern]


def test_a_word_is_a_letter_a_number_or_an_underscore() -> None:
    """`cargo test` reads `\\w` in SQLAlchemy's expressions as Python's `re` matches it, `[\\p{L}\\p{N}_]` by the
    general categories of this Python's Unicode database.
    """
    word = re.compile(r"\w")
    differ = [
        f"U+{code:04X}"
        for code in range(sys.maxunicode + 1)
        if not 0xD800 <= code <= 0xDFFF  # Surrogates are no characters
        and bool(word.fullmatch(c := chr(code))) != (c == "_" or unicodedata.category(c)[0] in "LN")
    ]
    assert len(differ) <= 0, differ[:10]
