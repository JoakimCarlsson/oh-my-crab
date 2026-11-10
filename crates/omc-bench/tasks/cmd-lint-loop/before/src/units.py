"""Converting between units of length."""

FACTORS = {"mm": 0.001, "cm": 0.01, "m": 1.0, "km": 1000.0, "in": 0.0254, "ft": 0.3048, "mi": 1609.344}


def to_metres(value, unit):
    return value * FACTORS[unit]


def convert(value, source, target):
    """Return ``value`` in the unit ``target``, given in the unit ``source``."""
    return to_metres(value, source) / FACTORS[target]


def known(unit):
    """Return whether ``unit`` can be converted."""
    return unit in FACTORS
