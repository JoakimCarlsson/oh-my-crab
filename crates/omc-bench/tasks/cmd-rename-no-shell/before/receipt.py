from money import get_total


def receipt_summary(items):
    return f"receipt: {get_total(items)}"
