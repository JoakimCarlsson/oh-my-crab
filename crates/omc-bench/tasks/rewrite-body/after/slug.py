"""Turning titles into the last part of a web address."""

import re
import unicodedata


def slugify(text, separator="-", limit=0):
    """Return ``text`` as lower-case words joined by ``separator``, cut to ``limit`` characters when it is not 0."""
    text = unicodedata.normalize("NFKD", text)
    text = text.encode("ascii", "ignore").decode("ascii")
    words = re.findall(r"[a-z0-9]+", text.lower())
    slug = separator.join(words)
    return slug[:limit].rstrip(separator) if limit else slug


def unique(slug, taken):
    """Return ``slug``, or ``slug`` with the lowest number after it that is not in ``taken``."""
    if slug not in taken:
        return slug
    number = 2
    while f"{slug}-{number}" in taken:
        number += 1
    return f"{slug}-{number}"
