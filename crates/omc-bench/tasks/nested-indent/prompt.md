In `report.py`, inside `collect`, add a new branch to the innermost `if`/`elif` chain: directly after the `elif entry.level == "warning":` branch and before the `else:` branch, add

```python
elif entry.level == "notice":
    if verbose:
        notices.append(entry.text)
```

indented to line up with the other branches of that chain. Change nothing else.
