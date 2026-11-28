from money import get_total


def cart_summary(items):
    return f"cart: {get_total(items)}"
