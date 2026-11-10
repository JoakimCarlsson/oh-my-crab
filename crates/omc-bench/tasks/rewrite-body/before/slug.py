"""Turning titles into the last part of a web address."""

import re
import unicodedata


def slugify(text, separator="-", limit=0):
    """Return ``text`` as lower-case words joined by ``separator``, cut to ``limit`` characters when it is not 0."""
    result = ""
    previous_was_separator = True
    for character in text.lower():
        if character.isalnum():
            result += character
            previous_was_separator = False
        elif not previous_was_separator:
            result += separator
            previous_was_separator = True
    if result.endswith(separator):
        result = result[: -len(separator)]
    if limit:
        result = result[:limit]
    return result


def unique(slug, taken):
    """Return ``slug``, or ``slug`` with the lowest number after it that is not in ``taken``."""
    if slug not in taken:
        return slug
    number = 2
    while f"{slug}-{number}" in taken:
        number += 1
    return f"{slug}-{number}"
