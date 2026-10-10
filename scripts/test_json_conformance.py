"""Regression checks for false passes in the external JSON acceptance harness."""

import hashlib
from io import BytesIO
from pathlib import Path
import sys
import tempfile
import unittest
from unittest.mock import patch

import check_json_conformance as check


class ConformanceTests(unittest.TestCase):
    def output(self, source, output):
        cases = [("i_probe.json", source)]
        return check.check_output(output, cases, check.expected_values(cases, {}))

    def test_numbers_preserve_kind_and_spelling(self):
        for output in [b'"1"\n', b'1.0\n', b'true\n']:
            with self.subTest(output=output), self.assertRaises(check.CheckError):
                self.output(b"1", output)
        self.assertEqual(self.output(b"-0.1200E+004", b"-0.1200E+004\n"), 1)

    def test_object_order_is_irrelevant_but_array_order_is_not(self):
        self.assertEqual(self.output(b'{"a":1,"b":null}', b'{"b":null,"a":1}\n'), 1)
        with self.assertRaises(check.CheckError):
            self.output(b"[1,2]", b"[2,1]\n")

    def test_unicode_separators_do_not_split_output_records(self):
        output = '"\u0085\u2028\u2029"\n'.encode()
        self.assertEqual(self.output(b'"\\u0085\\u2028\\u2029"', output), 1)

    def test_rejection_encoding_failure_and_truncated_output_fail(self):
        for output in [b"REJECT\n", b"ENCODE_ERROR\n", b"null", b"null\nnull\n"]:
            with self.subTest(output=output), self.assertRaises(check.CheckError):
                self.output(b"null", output)
        self.assertEqual(self.output(b"[", b"REJECT\n"), 0)
        with self.assertRaises(check.CheckError):
            self.output(b"[", b"null\n")

    def test_strict_policy_and_upstream_labels_are_checked(self):
        for source in [b'{"a":1,"a":2}', b'"\\ud800"', b'"\xff"', b'\xef\xbb\xbfnull', b"NaN"]:
            with self.subTest(source=source), self.assertRaises(ValueError):
                check.semantic_value(source)
        with self.assertRaises(check.CheckError):
            check.expected_values([("y_invalid.json", b"[")], {})
        with self.assertRaises(check.CheckError):
            check.expected_values([("n_valid.json", b"null")], {})

    def test_default_depth_boundary(self):
        check.semantic_value(b"[" * 128 + b"]" * 128)
        with self.assertRaises(ValueError):
            check.semantic_value(b"[" * 129 + b"]" * 129)

    def test_offline_corpus_rejects_corruption_missing_and_extra_files(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            path = root / "y_null.json"
            lock = {"files": {path.name: hashlib.sha256(b"null").hexdigest()}}
            with self.assertRaises(check.CheckError):
                check.corpus(lock, root, False)
            path.write_bytes(b"null")
            self.assertEqual(check.corpus(lock, root, False), [(path.name, b"null")])
            path.write_bytes(b"true")
            with self.assertRaises(check.CheckError):
                check.corpus(lock, root, False)
            path.write_bytes(b"null")
            (root / "extra.json").write_bytes(b"null")
            with self.assertRaises(check.CheckError):
                check.corpus(lock, root, False)

    def test_process_failure_and_timeout_keep_diagnostics(self):
        with tempfile.TemporaryDirectory() as directory:
            work = Path(directory)
            with self.assertRaises(check.CheckError):
                check.command([sys.executable, "-c", "print('failure'); exit(3)"], work, "failure")
            self.assertEqual((work / "failure.stdout").read_bytes(), b"failure\n")
            with self.assertRaises(check.CheckError):
                check.command([sys.executable, "-c", "import time; time.sleep(10)"], work, "timeout", 0.1)
            self.assertTrue((work / "timeout.stderr").is_file())

    def test_download_escapes_filename_and_reuses_verified_cache(self):
        name = "n_structure_trailing_#.json"
        data = b"[]#"
        lock = {"revision": "a" * 40, "files": {name: hashlib.sha256(data).hexdigest()}}
        with tempfile.TemporaryDirectory() as directory:
            with patch.object(check.urllib.request, "urlopen", return_value=BytesIO(data)) as fetch:
                self.assertEqual(check.corpus(lock, Path(directory), True), [(name, data)])
                self.assertTrue(fetch.call_args.args[0].endswith("trailing_%23.json"))
            with patch.object(check.urllib.request, "urlopen", side_effect=AssertionError("network used")):
                self.assertEqual(check.corpus(lock, Path(directory), True), [(name, data)])


if __name__ == "__main__":
    unittest.main()
