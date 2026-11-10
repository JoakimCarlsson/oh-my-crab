"""Small helpers for working with text."""


def squeeze(text):
    """Return ``text`` with every run of white space made one space."""
    return " ".join(text.split())


def indent(text, width=4):
    """Return ``text`` with every line that is not empty moved right by ``width`` spaces."""
    pad = " " * width
    return "\n".join(pad + line if line else line for line in text.split("\n"))


def truncate(text, limit, ending="..."):
    """Return ``text`` cut to ``limit`` characters, with ``ending`` in place of what was cut."""
    if len(text) <= limit:
        return text
    return text[: max(limit - len(ending), 0)] + ending


def initials(name):
    """Return the first letter of each word of ``name``, in capitals."""
    return "".join(word[0].upper() for word in name.split())
