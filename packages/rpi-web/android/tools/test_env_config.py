"""Endpoint config tests use reserved example domains only; no network or secrets."""
from pathlib import Path
import tempfile
import unittest

from env_config import DEFAULT, DEVELOPMENT, load_environment, normalize_origin, parse_dotenv


class EnvironmentConfigTests(unittest.TestCase):
    def test_comments_quotes_and_blank_optional(self):
        values = parse_dotenv(f'# configuration\n\n{DEFAULT} = "https://workspace.example"\n{DEVELOPMENT}=\n')
        self.assertEqual(values, {DEFAULT: "https://workspace.example", DEVELOPMENT: ""})
        self.assertEqual(parse_dotenv(f"{DEFAULT}='https://workspace.example'\n")[DEFAULT], "https://workspace.example")

    def test_rejects_unknown_keys_duplicates_shell_and_malformed_lines(self):
        for text in ["PASSWORD=secret", "export " + DEFAULT + "=https://workspace.example",
                     DEFAULT, f'{DEFAULT}=a\n{DEFAULT}=b', f'{DEFAULT}="unclosed',
                     f'{DEFAULT}=$(command)', f'{DEFAULT}=`command`', f'{DEFAULT}=${{HOME}}',
                     f'{DEFAULT}=https://workspace.example\\bad']:
            with self.subTest(text=text), self.assertRaises(ValueError):
                parse_dotenv(text)

    def test_missing_required_no_default_fallback(self):
        with tempfile.TemporaryDirectory() as directory:
            with self.assertRaisesRegex(ValueError, DEFAULT + " is required"):
                load_environment(Path(directory), {})

    def test_environment_precedes_file_including_explicit_blank_development(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / ".env.local").write_text(f'{DEFAULT}=https://file.example\n{DEVELOPMENT}=https://development.example\n')
            env = load_environment(root, {DEFAULT: "https://override.example/", DEVELOPMENT: "", "OTHER": "untouched"})
            self.assertEqual(env[DEFAULT], "https://override.example")
            self.assertEqual(env[DEVELOPMENT], "")
            self.assertEqual(env["OTHER"], "untouched")
            with self.assertRaises(ValueError):
                load_environment(root, {DEFAULT: ""})

    def test_file_load_is_optional_but_requires_trusted_origin(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            self.assertEqual(load_environment(root, {DEFAULT: "https://workspace.example"})[DEVELOPMENT], "")
            (root / ".env.local").write_text(f'{DEFAULT}=https://file.example\n', encoding="utf-8-sig")
            self.assertEqual(load_environment(root, {})[DEFAULT], "https://file.example")

    def test_origin_normalization(self):
        for raw, expected in [(" HTTPS://WORKSPACE.EXAMPLE:443/ ", "https://workspace.example"),
                              ("https://workspace.example:8443", "https://workspace.example:8443"),
                              ("https://[2001:db8::1]:8443/", "https://[2001:db8::1]:8443"),
                              ("https://192.168.1.5/", "https://192.168.1.5")]:
            self.assertEqual(normalize_origin(raw, DEFAULT, required=True), expected)

    def test_rejects_unsafe_origin_without_logging_value(self):
        invalid = ["http://workspace.example", "//workspace.example", "javascript:alert(1)",
                   "file:///test", "https://user:private-value@workspace.example", "https://workspace.example/path",
                   "https://workspace.example?", "https://workspace.example/#", "https://workspace.example:0",
                   "https://workspace.example:65536", "https://workspace.example:", "https://workspace.example.",
                   "https://work space.example", "https://éxample.com", "https://workspace.example\\@evil.example",
                   "https://%65xample.com", "https://workspace.example\n.evil.example", 'https://workspace.example"',
                   "https://workspace.example;command", "https://work_space.example",
                   "https://[2001:db8::1]workspace.example", "https://[::1]suffix:443",
                   "https://-workspace.example", "https://workspace-.example", "https://workspace..example",
                   "https://1.2.3.999", "https://workspace.123", "https://workspace.1example"]
        for raw in invalid:
            with self.subTest(raw=raw), self.assertRaises(ValueError) as error:
                normalize_origin(raw, DEFAULT, required=True)
            self.assertNotIn(raw, str(error.exception))
            self.assertNotIn("private-value", str(error.exception))

    def test_optional_invalid_value_is_still_rejected(self):
        self.assertEqual(normalize_origin("", DEVELOPMENT), "")
        with self.assertRaises(ValueError):
            normalize_origin("http://development.example", DEVELOPMENT)


if __name__ == "__main__":
    unittest.main()
