"""Tests for the order prices."""

import unittest

import pricing


def order(price, quantity):
    """Return an order of one kind of item."""
    return [{"price": price, "quantity": quantity}]


class PricingTest(unittest.TestCase):
    """What an order costs."""

    def test_subtotal_adds_up_every_item(self):
        """Every item counts with its quantity."""
        items = order(2.0, 3) + order(1.5, 2)
        self.assertEqual(pricing.subtotal(items), 9.0)

    def test_small_orders_get_no_volume_discount(self):
        """Fewer than ten units get nothing off."""
        self.assertEqual(pricing.volume_discount(order(1.0, 9)), 0.0)

    def test_a_tier_starts_at_its_threshold(self):
        """Exactly ten, twenty and fifty units reach their tiers."""
        self.assertEqual(pricing.volume_discount(order(1.0, 10)), 0.05)
        self.assertEqual(pricing.volume_discount(order(1.0, 20)), 0.10)
        self.assertEqual(pricing.volume_discount(order(1.0, 50)), 0.15)

    def test_a_coupon_never_takes_more_than_the_amount(self):
        """A coupon worth more than the order makes it free, not negative."""
        self.assertEqual(pricing.coupon_value("SPRING", 4.0), 4.0)

    def test_shipping_is_free_from_the_limit(self):
        """An order worth the limit ships free; one below it does not."""
        self.assertEqual(pricing.shipping(100.0), 0.0)
        self.assertEqual(pricing.shipping(99.99), 7.5)

    def test_total_takes_everything_into_account(self):
        """Discount, coupon and shipping all show in the total."""
        self.assertEqual(pricing.total(order(4.0, 5), "WELCOME"), 22.5)


if __name__ == "__main__":
    unittest.main()
