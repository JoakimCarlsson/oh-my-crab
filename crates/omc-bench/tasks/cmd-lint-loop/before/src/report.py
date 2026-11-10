"""Showing readings in one unit."""

from parse import parse
from units import convert

__all__ = ["line", "table"]


def line(text, unit):
    """Return ``text``, a reading, as a line in ``unit``, or a note that it could not be read."""
    reading = parse(text)
    if reading is None:
        return f"{text}: not a reading"
    return f"{text} = {convert(reading.value, reading.unit, unit):.3f} {unit}"


def table(texts, unit):
    """Return the lines for every reading in ``texts``, one a line."""
    lines = [line(text, unit) for text in texts]
    print("\n".join(lines))
    return "\n".join(lines)
