"""Check that every value crossing a generated function matches its annotation.

SQLAlchemy rows from `text()` are `Any`, so the type checkers cannot see what a
driver really returns; this is the layer that can. It is stricter than
typeguard: it descends into dataclass fields and list items, rejects `bool`
where `int` is declared and `datetime` where `date` is, and requires enum
members rather than their string values.
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


def check(value: object, hint: Any, path: str = "value") -> None:  # noqa: ANN401, C901
    """Fail with the offending path unless `value` inhabits `hint` exactly.

    `object` is what a JSON column is annotated: any value inhabits it, so
    whether a driver decoded the JSON is `typemap.py`'s to check.
    """
    origin = typing.get_origin(hint)
    if hint is object:
        return
    if origin in (typing.Union, types.UnionType):
        options = typing.get_args(hint)
        if value is None:
            assert type(None) in options, f"{path}: None for non-optional {hint}"
            return
        failures = [failure(value, o, path) for o in options if o is not type(None)]
        assert None in failures, "; ".join(map(str, failures))
    # `Iterable` is `:copyfrom` input; tests pass lists.
    elif origin in (list, Iterable):
        if origin is list:
            assert type(value) is list, f"{path}: {type(value).__name__} is not a list"
        (item,) = typing.get_args(hint)
        for i, element in enumerate(typing.cast("list[object]", value)):
            check(element, item, f"{path}[{i}]")
    elif origin is not None:
        assert isinstance(value, origin), f"{path}: {value!r} is not {hint}"
    elif dataclasses.is_dataclass(hint):
        assert type(value) is hint, f"{path}: {type(value).__name__} is not {hint}"
        fields = typing.get_type_hints(hint)
        for field in dataclasses.fields(hint):
            check(
                getattr(value, field.name), fields[field.name], f"{path}.{field.name}"
            )
    # The one tolerated subclass: asyncpg returns its own `uuid.UUID` subclass.
    elif isinstance(hint, type) and issubclass(hint, uuid.UUID):
        assert isinstance(value, hint), f"{path}: {value!r} is not {hint.__name__}"
    else:
        assert type(value) is hint, (
            f"{path}: {type(value).__name__} {value!r} is not {hint}"
        )


def failure(value: object, hint: Any, path: str = "value") -> str | None:  # noqa: ANN401
    """Say why `value` does not inhabit `hint`, or None if it does."""
    try:
        check(value, hint, path)
    except AssertionError as failure:
        return str(failure)
    return None


def checked(fn: Callable[..., Any], called: set[str]) -> Callable[..., Any]:
    """Wrap a generated function so each call checks arguments and result."""
    hints = typing.get_type_hints(fn)  # also proves the hints resolve at runtime
    signature = inspect.signature(fn)
    name = f"{fn.__module__}.{fn.__qualname__}"

    def check_arguments(*args: object, **kwargs: object) -> None:
        bound = signature.bind(*args, **kwargs)
        for parameter, value in bound.arguments.items():
            check(value, hints[parameter], f"{name}({parameter}=)")
        called.add(name)

    if inspect.iscoroutinefunction(fn):

        @functools.wraps(fn)
        async def async_wrapper(*args: object, **kwargs: object) -> object:
            check_arguments(*args, **kwargs)
            result = await fn(*args, **kwargs)
            check(result, hints["return"], f"{name}() ->")
            return result

        return async_wrapper

    @functools.wraps(fn)
    def wrapper(*args: object, **kwargs: object) -> object:
        check_arguments(*args, **kwargs)
        result = fn(*args, **kwargs)
        check(result, hints["return"], f"{name}() ->")
        return result

    return wrapper


def strip_none(hint: Any) -> tuple[Any, bool]:  # noqa: ANN401
    """`T`, and whether the hint was `T | None`."""
    args = typing.get_args(hint)
    if type(None) not in args:
        return hint, False
    rest = [a for a in args if a is not type(None)]
    return (rest[0] if len(rest) == 1 else typing.Union[tuple(rest)]), True  # noqa: UP007


def build(model: Any, row: Sequence[Any]) -> object:  # noqa: ANN401
    """Build a model by position, as `from_row` does (docs/python-output.md §4),
    to hold `from_row` to it.
    """
    hints = typing.get_type_hints(model)
    fields = dataclasses.fields(model)
    assert len(fields) == len(row), f"{model.__name__}: {len(row)} columns"
    return model(
        **{f.name: coerce(v, hints[f.name]) for f, v in zip(fields, row, strict=True)}
    )


def coerce(value: object, hint: Any) -> object:  # noqa: ANN401
    """Apply `from_row`'s one conversion: both drivers return enums as `str`."""
    origin = typing.get_origin(hint)
    if value is None:
        return None
    if isinstance(hint, type) and issubclass(hint, enum.Enum):
        return hint(value)
    if origin is list:
        (item,) = typing.get_args(hint)
        return [coerce(v, item) for v in typing.cast("list[object]", value)]
    if origin in (typing.Union, types.UnionType):
        rest = [a for a in typing.get_args(hint) if a is not type(None)]
        return coerce(value, rest[0]) if len(rest) == 1 else value
    return value
