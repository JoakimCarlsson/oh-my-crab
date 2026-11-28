from money import get_total


def refund_summary(items):
    return f"refund: {get_total(items)}"
