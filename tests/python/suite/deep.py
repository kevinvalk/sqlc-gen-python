"""Checks that every value crossing a generated function matches its annotation.

SQLAlchemy rows from `text()` are `Any`, so the type checkers cannot see what a driver really returns and this is the
layer that can. We are stricter than typeguard on purpose: we descend into dataclass fields and list items, reject
`bool` where `int` is declared and `datetime` where `date` is, and require enum members rather than their values.
"""

import dataclasses
import enum
import functools
import inspect
import types
import typing
import uuid
from collections.abc import Callable, Iterable, Sequence
from typing import Any


def check(value: object, hint: Any, path: str = "value") -> None:  # noqa: C901
    """Fail with the offending path unless `value` inhabits `hint` exactly."""
    origin = typing.get_origin(hint)

    # A JSON column is annotated `object`, which any value inhabits, so whether a driver decoded the JSON is for
    # `typemap.py` to check.
    if hint is object:
        return
    if origin in (typing.Union, types.UnionType):
        options = typing.get_args(hint)
        if value is None:
            assert type(None) in options, f"{path}: None for non-optional {hint}"
            return
        failures = [failure(value, option, path) for option in options if option is not type(None)]
        assert None in failures, "; ".join(map(str, failures))
    # `Iterable` is what a `:copyfrom` takes, and our tests pass lists.
    elif origin in (list, Iterable):
        if origin is list:
            assert type(value) is list, f"{path}: {type(value).__name__} is not a list"
        (item,) = typing.get_args(hint)
        for i, element in enumerate(typing.cast("list[object]", value)):
            check(element, item, f"{path}[{i}]")
    elif origin is not None:
        assert isinstance(value, origin), f"{path}: {value!r} is not {hint}"
        bounds(value, hint, path)
    elif dataclasses.is_dataclass(hint):
        assert type(value) is hint, f"{path}: {type(value).__name__} is not {hint}"
        fields = typing.get_type_hints(hint)
        for field in dataclasses.fields(hint):
            check(getattr(value, field.name), fields[field.name], f"{path}.{field.name}")
    # The one subclass we tolerate, as asyncpg returns its own subclass of `uuid.UUID`.
    elif isinstance(hint, type) and issubclass(hint, uuid.UUID):
        assert isinstance(value, hint), f"{path}: {value!r} is not {hint.__name__}"
    else:
        assert type(value) is hint, f"{path}: {type(value).__name__} {value!r} is not {hint}"


def bounds(value: object, hint: Any, path: str) -> None:
    """Check what a generic value holds: the keys and items of a dict (an `hstore`), or the bounds of a psycopg or
    SQLAlchemy range, each of the range's type or open (`None`).
    """
    if isinstance(value, dict):
        key_hint, item_hint = typing.get_args(hint)
        for key, item in typing.cast("dict[object, object]", value).items():
            check(key, key_hint, f"{path} key")
            check(item, item_hint, f"{path}[{key!r}]")
        return

    if not (hasattr(value, "lower") and hasattr(value, "upper")):
        return

    (bound,) = typing.get_args(hint)
    for end in ("lower", "upper"):
        if (limit := getattr(value, end)) is not None:
            check(limit, bound, f"{path}.{end}")


def failure(value: object, hint: Any, path: str = "value") -> str | None:
    """Why `value` does not inhabit `hint`, or None if it does."""
    try:
        check(value, hint, path)
    except AssertionError as failure:
        return str(failure)
    return None


def checked(function: Callable[..., Any], called: set[str]) -> Callable[..., Any]:
    """Wrap a generated function so each call checks its arguments and result, and records the function in `called`."""
    hints = typing.get_type_hints(function)  # This also proves the hints resolve at runtime
    signature = inspect.signature(function)
    name = f"{function.__module__}.{function.__qualname__}"

    def check_arguments(*args: object, **kwargs: object) -> None:
        bound = signature.bind(*args, **kwargs)
        for parameter, value in bound.arguments.items():
            check(value, hints[parameter], f"{name}({parameter}=)")
        called.add(name)

    if inspect.iscoroutinefunction(function):

        @functools.wraps(function)
        async def async_wrapper(*args: object, **kwargs: object) -> object:
            check_arguments(*args, **kwargs)
            result = await function(*args, **kwargs)
            check(result, hints["return"], f"{name}() ->")
            return result

        return async_wrapper

    @functools.wraps(function)
    def wrapper(*args: object, **kwargs: object) -> object:
        check_arguments(*args, **kwargs)
        result = function(*args, **kwargs)
        check(result, hints["return"], f"{name}() ->")
        return result

    return wrapper


def strip_none(hint: Any) -> tuple[Any, bool]:
    """`T`, and whether the hint was `T | None`."""
    arguments = typing.get_args(hint)
    if type(None) not in arguments:
        return hint, False

    rest = [argument for argument in arguments if argument is not type(None)]
    return (rest[0] if len(rest) == 1 else typing.Union[tuple(rest)]), True  # noqa: UP007


def build(model: Any, row: Sequence[Any]) -> object:
    """Build a model by position, as `from_row` does, so we can hold `from_row` to it."""
    hints = typing.get_type_hints(model)
    fields = dataclasses.fields(model)
    assert len(fields) == len(row), f"{model.__name__}: {len(row)} columns"
    return model(**{field.name: coerce(value, hints[field.name]) for field, value in zip(fields, row, strict=True)})


def coerce(value: object, hint: Any) -> object:
    """Apply the one conversion `from_row` does, as both drivers return enums as `str`."""
    origin = typing.get_origin(hint)
    if value is None:
        return None

    if isinstance(hint, type) and issubclass(hint, enum.Enum):
        return hint(value)
    if origin is list:
        (item,) = typing.get_args(hint)
        if isinstance(value, str):
            # NOTE: psycopg returns an array of a type it does not know (an enum's) as its text `{a,b}`, whose labels
            # need no quoting.
            value = [label for label in value[1:-1].split(",") if label != ""]
        return [coerce(element, item) for element in typing.cast("list[object]", value)]
    if origin in (typing.Union, types.UnionType):
        rest = [argument for argument in typing.get_args(hint) if argument is not type(None)]
        return coerce(value, rest[0]) if len(rest) == 1 else value
    return value
