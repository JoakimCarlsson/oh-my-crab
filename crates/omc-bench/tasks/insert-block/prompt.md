In `textutil.py`, add this function directly after `squeeze` and before `truncate`, with two blank lines on each side of it. Change nothing else.

```python
def indent(text, width=4):
    """Return ``text`` with every line that is not empty moved right by ``width`` spaces."""
    pad = " " * width
    return "\n".join(pad + line if line else line for line in text.split("\n"))
```
