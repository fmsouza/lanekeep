import unittest

from lanekeep import glob_matches


class TestGlobMatches(unittest.TestCase):
    def test_a_pattern_is_anchored_at_both_ends(self):
        self.assertFalse(glob_matches("super", "super::*"))
        self.assertTrue(glob_matches("super::*", "super::*"))
        self.assertFalse(glob_matches("subject/*.rs", "subject/input.rs.bak"))
        self.assertFalse(glob_matches("subject/*.rs", "xsubject/input.rs"))

    def test_a_star_spans_a_path_segment(self):
        self.assertTrue(glob_matches("subject/*.rs", "subject/input.rs"))

    def test_a_star_does_not_span_a_line_terminator(self):
        self.assertFalse(glob_matches("*prelude*", "std::\n    prelude::*"))
        self.assertFalse(glob_matches("*prelude*", "std::\r\n    prelude::*"))
        self.assertTrue(glob_matches("*prelude*", "std::prelude::*"))

    def test_a_regex_metacharacter_in_a_pattern_is_a_literal(self):
        self.assertFalse(glob_matches("a.rs", "axrs"))
        self.assertTrue(glob_matches("a.rs", "a.rs"))


if __name__ == "__main__":
    unittest.main()
