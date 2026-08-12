import unittest

from lanekeep import capture


class Entry:
    def __init__(self, name, node):
        self.name = name
        self.node = node


class TestCapture(unittest.TestCase):
    def test_a_capture_that_did_not_participate_is_absent_rather_than_null(self):
        entries = [Entry("call", 7)]
        self.assertEqual(capture(entries, "call"), 7)
        self.assertIsNone(capture(entries, "method"))

    def test_the_root_handle_zero_is_a_hit_not_a_miss(self):
        entries = [Entry("root", 0)]
        self.assertEqual(capture(entries, "root"), 0)
        self.assertIsNone(capture(entries, "other"))


if __name__ == "__main__":
    unittest.main()
