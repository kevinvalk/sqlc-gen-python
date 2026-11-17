"""Telling a refusal from a failure in the runtime sweep (see ``autoprop``).

A call that raises fails its package, unless the error says nothing about the generated code: the database or the
driver refused the drawn data, or PostgreSQL rejects sqlc's SQL as written. Such a call is inconclusive.
"""

from __future__ import annotations

import psycopg
import sqlalchemy

# SQLSTATE classes that mean the database refused the drawn data, not the call: cardinality, data, integrity,
# transaction state, rollback, insufficient resources (memory), program limits (an array too large), object state,
# operator intervention (timeouts), PL/pgSQL raise, feature not supported.
REFUSALS = ("21", "22", "23", "25", "40", "53", "54", "55", "57", "P0", "0A")


def refused(error: Exception) -> bool:
    """Whether the database or driver refused the drawn data, not the call: a constraint or data error, a value outside
    Python's range, a cardinality or timeout error, an error raised inside a function the schema defines, or a syntax
    error in a value (``to_tsquery('a:')``), which unlike one in the statement points nowhere into it.
    """
    if isinstance(error, OverflowError) or is_error(error, sqlalchemy.exc.MultipleResultsFound, "MultipleRowsError"):
        return True
    if isinstance(error, (sqlalchemy.exc.IntegrityError, sqlalchemy.exc.DataError)):
        return True
    driver_error = database_error(error)
    if driver_error is None:
        return False

    # psycopg refuses some data itself, before PostgreSQL could (a NUL in text).
    if isinstance(driver_error, psycopg.DataError) and sqlstate(driver_error) is None:
        return True

    # We read psycopg's diagnostics, or the asyncpg error SQLAlchemy's adapter wraps.
    diagnostic = getattr(driver_error, "diag", None)
    cause = getattr(driver_error, "__cause__", None)
    context = getattr(diagnostic, "context", None) or getattr(cause, "context", None)
    position = getattr(diagnostic, "statement_position", None) or getattr(cause, "position", None)
    state = sqlstate(driver_error) or ""
    return state[:2] in REFUSALS or "function" in (context or "") or (state == "42601" and not position)


def is_error(error: Exception, sqlalchemy_error: type[Exception], package: str) -> bool:
    """Whether ``error`` is SQLAlchemy's error, or the generated package's of that name (a class of each package's own
    models).
    """
    return isinstance(error, sqlalchemy_error) or (type(error).__name__ == package and isinstance(error, LookupError))


def database_error(error: Exception) -> BaseException | None:
    """The driver's error behind ``error``, which SQLAlchemy wraps and a psycopg package raises as is.

    NOTE: SQLAlchemy's asyncpg adapter raises asyncpg's own error as is where a server-side cursor fetches, as a stream
    does, since it translates none there (SQLAlchemy 2.0.57 and 2.1.8).
    """
    if isinstance(error, sqlalchemy.exc.DBAPIError):
        return error.orig
    if type(error).__module__.startswith("asyncpg."):
        return error
    return error if isinstance(error, psycopg.Error) else None


def sqlstate(error: BaseException) -> str | None:
    found = getattr(error, "sqlstate", None) or getattr(error, "pgcode", None)  # psycopg's, or asyncpg's
    return str(found) if found else None


def rejected(error: Exception, url: str, sql: str) -> bool:
    """Whether a syntax or access error is PostgreSQL rejecting sqlc's SQL as written, whatever the arguments, because
    it cannot even plan it.
    """
    driver_error = database_error(error)
    if driver_error is None or (sqlstate(driver_error) or "")[:2] != "42":
        return False

    with psycopg.connect(url, autocommit=True) as connection:
        try:
            connection.execute(f"EXPLAIN (GENERIC_PLAN) {sql}")
        except psycopg.Error:
            return True
    return False
