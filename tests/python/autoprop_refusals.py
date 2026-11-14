"""Telling a refusal from a failure in the runtime sweep (see ``autoprop``).

A call that raises fails its package, unless the error says nothing about the
generated code: the database or the driver refused the data that was drawn, or
PostgreSQL rejects sqlc's SQL as written. Such a call is inconclusive.
"""

from __future__ import annotations

import psycopg
import sqlalchemy

# SQLSTATE classes that mean the database refused the drawn data, not the call:
# cardinality, data, integrity, transaction state, rollback, insufficient
# resources (memory), program limits (an array too large), object state,
# operator intervention (timeouts), PL/pgSQL raise, feature not supported.
REFUSALS = ("21", "22", "23", "25", "40", "53", "54", "55", "57", "P0", "0A")


def refused(error: Exception) -> bool:
    """Whether the database or driver refused the drawn data, not the call:
    a constraint or data error, a value outside Python's range, a cardinality
    or timeout error, an error raised inside a function the schema defines, or
    a syntax error in a value (``to_tsquery('a:')``), which unlike one in the
    statement points nowhere into it.
    """
    if isinstance(error, OverflowError) or is_error(
        error, sqlalchemy.exc.MultipleResultsFound, "MultipleRowsError"
    ):
        return True
    if isinstance(error, (sqlalchemy.exc.IntegrityError, sqlalchemy.exc.DataError)):
        return True
    orig = database_error(error)
    if orig is None:
        return False
    # psycopg refuses some data itself, before PostgreSQL could (a NUL in text).
    if isinstance(orig, psycopg.DataError) and sqlstate(orig) is None:
        return True
    # psycopg's diagnostics, or the asyncpg error SQLAlchemy's adapter wraps.
    diag = getattr(orig, "diag", None)
    cause = getattr(orig, "__cause__", None)
    context = getattr(diag, "context", None) or getattr(cause, "context", None)
    position = getattr(diag, "statement_position", None) or getattr(
        cause, "position", None
    )
    state = sqlstate(orig) or ""
    return (
        state[:2] in REFUSALS
        or "function" in (context or "")
        or (state == "42601" and not position)
    )


def is_error(error: Exception, sqlalchemy_error: type[Exception], package: str) -> bool:
    """Whether ``error`` is SQLAlchemy's error, or the generated package's of
    that name (a class of each package's own ``__init__``).
    """
    return isinstance(error, sqlalchemy_error) or (
        type(error).__name__ == package and isinstance(error, LookupError)
    )


def database_error(error: Exception) -> BaseException | None:
    """The driver's error behind ``error``: SQLAlchemy's wraps it, a psycopg
    package raises it; ``None`` if it is no database error.
    """
    if isinstance(error, sqlalchemy.exc.DBAPIError):
        return error.orig
    return error if isinstance(error, psycopg.Error) else None


def sqlstate(error: BaseException) -> str | None:
    """The SQLSTATE of a driver's error, from psycopg or asyncpg."""
    found = getattr(error, "sqlstate", None) or getattr(error, "pgcode", None)
    return str(found) if found else None


def rejected(error: Exception, url: str, sql: str) -> bool:
    """Whether a syntax or access error is PostgreSQL rejecting sqlc's SQL as
    written, whatever the arguments: it cannot even plan it.
    """
    orig = database_error(error)
    if orig is None or (sqlstate(orig) or "")[:2] != "42":
        return False
    with psycopg.connect(url, autocommit=True) as conn:
        try:
            conn.execute(f"EXPLAIN (GENERIC_PLAN) {sql}")
        except psycopg.Error:
            return True
    return False
