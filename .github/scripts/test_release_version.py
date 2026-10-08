"""Exercise release preparation in disposable package directories."""

import importlib.util
import os
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch


spec = importlib.util.spec_from_file_location("release_version", Path(__file__).with_name("release-version.py"))
release = importlib.util.module_from_spec(spec)
spec.loader.exec_module(release)

MANIFEST = '[package]\nname = "xmux"\nversion = "0.17.7"\nedition = "2021"\n\n[dependencies]\nexample = "0.17.7"\n'
LOCK = 'version = 4\n\n[[package]]\nname = "example"\nversion = "0.17.7"\nsource = "registry+https://example.com"\n\n[[package]]\nname = "xmux"\nversion = "0.17.7"\n'


class ReleaseVersionTests(unittest.TestCase):
    def setUp(self):
        previous = Path.cwd()
        directory = tempfile.TemporaryDirectory()
        self.addCleanup(directory.cleanup)
        self.addCleanup(os.chdir, previous)
        os.chdir(directory.name)
        Path("Cargo.toml").write_text(MANIFEST)
        Path("Cargo.lock").write_text(LOCK)
        self.tags = patch.object(release.subprocess, "check_output", return_value="v0.17.7\nv0.18.0-rc.1\n").start()
        self.addCleanup(patch.stopall)

    def test_prepare_updates_only_local_package_versions(self):
        self.assertEqual(release.release_version("prepare", "0.18.0", "main"), "v0.18.0")
        self.assertEqual(release.package_versions(), ("0.18.0", "0.18.0"))
        self.assertIn('example = "0.17.7"', Path("Cargo.toml").read_text())
        self.assertIn('name = "example"\nversion = "0.17.7"', Path("Cargo.lock").read_text())

    def test_check_accepts_matching_tag_without_writes(self):
        self.assertEqual(release.release_version("check", "", "v0.17.7"), "v0.17.7")
        self.assertEqual(Path("Cargo.toml").read_text(), MANIFEST)
        self.assertEqual(Path("Cargo.lock").read_text(), LOCK)
        self.tags.assert_not_called()

    def test_check_rejects_either_mismatched_file(self):
        for filename, original in (("Cargo.toml", MANIFEST), ("Cargo.lock", LOCK)):
            with self.subTest(filename=filename):
                Path(filename).write_text(original.replace('version = "0.17.7"', 'version = "0.18.0"'))
                with self.assertRaises(ValueError):
                    release.release_version("check", "", "v0.17.7")
                Path(filename).write_text(original)

    def test_prepare_repairs_stale_lock_version(self):
        Path("Cargo.lock").write_text(LOCK.replace('version = "0.17.7"', 'version = "0.16.0"'))
        release.release_version("prepare", "0.18.0", "main")
        self.assertEqual(release.package_versions(), ("0.18.0", "0.18.0"))

    def test_invalid_or_non_increasing_versions_leave_files_untouched(self):
        for version in ("", "v0.18.0", "0.18", "00.18.0", "0.18.0-rc.1", "0.18.0\n", "$(id)", "0.17.7", "0.16.9"):
            with self.subTest(version=version), self.assertRaises(ValueError):
                release.release_version("prepare", version, "main")
            self.assertEqual(Path("Cargo.toml").read_text(), MANIFEST)
            self.assertEqual(Path("Cargo.lock").read_text(), LOCK)

    def test_existing_or_newer_tag_prevents_preparation(self):
        for tag in ("v0.18.0", "v0.19.0"):
            self.tags.return_value = tag + "\n"
            with self.assertRaises(ValueError):
                release.release_version("prepare", "0.18.0", "main")
            self.assertEqual(Path("Cargo.toml").read_text(), MANIFEST)

    def test_unsupported_lock_layout_does_not_partially_write_manifest(self):
        Path("Cargo.lock").write_text(LOCK.replace('name = "xmux"\nversion', 'name = "xmux"\n# local\nversion'))
        with self.assertRaises(ValueError):
            release.release_version("prepare", "0.18.0", "main")
        self.assertEqual(Path("Cargo.toml").read_text(), MANIFEST)

    def test_invalid_mode_and_tag_are_rejected(self):
        for mode, tag in (("invalid", "v0.17.7"), ("check", "0.17.7"), ("check", "v0.17.7-rc.1")):
            with self.subTest(mode=mode, tag=tag), self.assertRaises(ValueError):
                release.release_version(mode, "0.18.0", tag)


if __name__ == "__main__":
    unittest.main()
