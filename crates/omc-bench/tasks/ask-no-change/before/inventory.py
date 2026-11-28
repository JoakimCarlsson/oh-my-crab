"""Stock levels for a small warehouse."""

from dataclasses import dataclass


@dataclass
class Item:
    """One kind of thing on the shelves."""

    name: str
    count: int
    threshold: int = 5


def low(items, threshold=5):
    """Return the items whose count is below ``threshold``."""
    return [item for item in items if item.count < threshold]


def restock(items, threshold=5, batch=20):
    """Raise every item below ``threshold`` by ``batch`` and return how many were raised."""
    raised = 0
    for item in items:
        if item.count < threshold:
            item.count += batch
            raised += 1
    return raised


def audit(items, threshold=5):
    """Return a line for each item, marked when it is below ``threshold``."""
    lines = []
    for item in items:
        mark = "!" if item.count < threshold else " "
        lines.append(f"{mark} {item.name}: {item.count}")
    return lines
