"""Rewrites a settings file in one shape: lower-case keys, sorted in each section, a blank line between sections."""

import sys


def parse(text):
    """Return the sections of ``text`` in the order they appear, each with its keys and values."""
    sections = {}
    current = None
    for line in text.split("\n"):
        line = line.strip()
        if not line or line.startswith("#"):
            continue
        if line.startswith("[") and line.endswith("]"):
            current = sections.setdefault(line[1:-1].strip().lower(), {})
        elif "=" in line and current is not None:
            key, value = line.split("=", 1)
            current[key.strip().lower()] = value.strip()
    return sections


def render(sections):
    """Return ``sections`` as text: each section's keys sorted, one blank line between sections."""
    blocks = []
    for name, values in sections.items():
        lines = [f"[{name}]"] + [f"{key} = {values[key]}" for key in sorted(values)]
        blocks.append("\n".join(lines))
    return "\n\n".join(blocks) + "\n"


def main():
    """Rewrite the file named on the command line in place."""
    path = sys.argv[1]
    with open(path) as source:
        sections = parse(source.read())
    with open(path, "w") as target:
        target.write(render(sections))


if __name__ == "__main__":
    main()
