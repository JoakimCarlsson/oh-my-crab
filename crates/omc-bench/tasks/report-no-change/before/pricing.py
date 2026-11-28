"""Prices for an order: a subtotal, a discount for volume, a coupon, and shipping."""

FREE_SHIPPING_FROM = 100.0
SHIPPING = 7.5

VOLUME_TIERS = [
    (50, 0.15),
    (20, 0.10),
    (10, 0.05),
]

COUPONS = {
    "WELCOME": 5.0,
    "SPRING": 12.0,
}


def subtotal(items):
    """Return the price of every item times its quantity, added up."""
    return sum(item["price"] * item["quantity"] for item in items)


def volume_discount(items):
    """Return the share taken off for the number of units ordered, from the highest tier reached."""
    units = sum(item["quantity"] for item in items)
    for threshold, share in VOLUME_TIERS:
        if units > threshold:
            return share
    return 0.0


def coupon_value(code, amount):
    """Return what the coupon ``code`` takes off ``amount``, never more than the amount itself."""
    return min(COUPONS.get(code, 0.0), amount)


def shipping(amount):
    """Return the shipping cost for an order worth ``amount`` after discounts."""
    return 0.0 if amount >= FREE_SHIPPING_FROM else SHIPPING


def total(items, code=None):
    """Return what the order costs: the subtotal less the volume discount and the coupon, plus shipping."""
    amount = subtotal(items)
    amount -= amount * volume_discount(items)
    amount -= coupon_value(code, amount)
    return round(amount + shipping(amount), 2)
