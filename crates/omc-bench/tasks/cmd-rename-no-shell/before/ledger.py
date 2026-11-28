from money import get_total


def ledger_summary(items):
    return f"ledger: {get_total(items)}"
