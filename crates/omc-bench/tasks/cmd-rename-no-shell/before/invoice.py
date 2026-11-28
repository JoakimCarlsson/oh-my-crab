from money import get_total


def invoice_summary(items):
    return f"invoice: {get_total(items)}"
