import unittest

from lanekeep import Handlers


class TestHandlers(unittest.TestCase):
    def test_metadata_is_required_and_delegates(self):
        called = []

        def metadata():
            called.append("metadata")
            return {"id": "probe"}

        handlers = Handlers(metadata=metadata)
        self.assertEqual(handlers.metadata(), {"id": "probe"})
        self.assertEqual(called, ["metadata"])

    def test_optional_entry_points_default_to_absent(self):
        handlers = Handlers(metadata=lambda: None)
        self.assertFalse(handlers.has_check())
        self.assertFalse(handlers.has_reduce())
        with self.assertRaises(RuntimeError):
            handlers.check(None, None)
        with self.assertRaises(RuntimeError):
            handlers.reduce(None)

    def test_configure_accepts_when_none_declared(self):
        handlers = Handlers(metadata=lambda: None)
        self.assertIsNone(handlers.configure("null"))

    def test_check_and_reduce_delegate(self):
        seen = {}

        def check(ctx, m):
            seen["check"] = (ctx, m)

        def reduce(ctx):
            seen["reduce"] = ctx

        handlers = Handlers(metadata=lambda: None, check=check, reduce=reduce)
        self.assertTrue(handlers.has_check())
        self.assertTrue(handlers.has_reduce())
        handlers.check("ctx", "m")
        handlers.reduce("ctx")
        self.assertEqual(seen, {"check": ("ctx", "m"), "reduce": "ctx"})

    def test_configure_delegates(self):
        seen = []

        def configure(options_json):
            seen.append(options_json)

        handlers = Handlers(metadata=lambda: None, configure=configure)
        handlers.configure('{"a": 1}')
        self.assertEqual(seen, ['{"a": 1}'])


if __name__ == "__main__":
    unittest.main()
