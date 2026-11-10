from units import known

__all__ = ["parse", "Reading"]


class Reading:
    def __init__(self, value, unit):
        """Keep the number and the unit it was given in."""
        self.value = value
        self.unit = unit


def parse(text):
    """Return the reading ``text`` holds, such as ``12.5 cm``, or nothing when it holds none."""
    parts = text.split()
    if len(parts) != 2:
        return None
    try:
        value = float(parts[0])
    except:
        return None
    if not known(parts[1]):
        return None
    return Reading(value, parts[1])
