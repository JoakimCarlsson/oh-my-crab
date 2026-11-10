"""Collecting what a batch of log files said."""


def collect(batches, verbose=False):
    """Return the errors, warnings and notices of every readable log in ``batches``, and how many entries were skipped."""
    errors, warnings, notices = [], [], []
    skipped = 0
    for batch in batches:
        if not batch.enabled:
            continue
        for log in batch.logs:
            try:
                entries = log.read()
            except OSError:
                skipped += 1
                continue
            for entry in entries:
                if entry.muted:
                    skipped += 1
                else:
                    if entry.level == "error":
                        errors.append(entry.text)
                    elif entry.level == "warning":
                        warnings.append(entry.text)
                    elif entry.level == "notice":
                        if verbose:
                            notices.append(entry.text)
                    else:
                        skipped += 1
    return errors, warnings, notices, skipped


def headline(errors, warnings):
    """Return one line that says how many errors and warnings there were."""
    if errors:
        return f"{len(errors)} errors, {len(warnings)} warnings"
    elif warnings:
        return f"{len(warnings)} warnings"
    else:
        return "clean"
