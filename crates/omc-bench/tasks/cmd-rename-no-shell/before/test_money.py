import unittest

from money import get_total


class Total(unittest.TestCase):
    def test_total(self):
        self.assertEqual(get_total([{"price": 2, "qty": 3}]), 6)
