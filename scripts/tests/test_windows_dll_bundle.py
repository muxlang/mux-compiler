import importlib.util
import subprocess
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

SCRIPT = Path(__file__).resolve().parents[1] / "ci" / "bundle-windows-dlls.py"
SPEC = importlib.util.spec_from_file_location("windows_dll_bundle", SCRIPT)
bundler = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(bundler)


class WindowsDllBundleTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.bin = self.root / "bin"
        self.llvm = self.root / "llvm"
        self.system = self.root / "system"
        for directory in (self.bin, self.llvm, self.system):
            directory.mkdir()
        (self.bin / "mux-driver.exe").write_bytes(b"driver")

    def test_transitive_imports_are_copied_case_insensitively_once(self):
        for name in ("LLVM-C.dll", "libxml2.dll", "zlib.dll"):
            (self.llvm / name).write_bytes(name.encode())
        (self.system / "KERNEL32.dll").write_bytes(b"system")
        dependencies = {
            "mux-driver.exe": [
                "llvm-c.DLL",
                "KERNEL32.dll",
                "api-ms-win-core-test.dll",
            ],
            "LLVM-C.dll": ["libxml2.dll", "zlib.dll"],
            "libxml2.dll": ["ZLIB.DLL"],
            "zlib.dll": ["LLVM-C.dll"],
        }
        copied = bundler.bundle(
            self.bin, [self.llvm], self.system, lambda binary: dependencies[binary.name]
        )
        self.assertEqual(set(copied), {"LLVM-C.dll", "libxml2.dll", "zlib.dll"})
        self.assertEqual(len(copied), 3)
        self.assertFalse((self.bin / "KERNEL32.dll").exists())
        self.assertEqual((self.bin / "zlib.dll").read_bytes(), b"zlib.dll")

    def test_missing_dependency_reports_importer(self):
        with self.assertRaisesRegex(
            FileNotFoundError, "missing.dll, imported by.*mux-driver"
        ):
            bundler.bundle(
                self.bin, [self.llvm], self.system, lambda _: ["missing.dll"]
            )

    def test_conflicting_sources_fail(self):
        other = self.root / "other"
        other.mkdir()
        (other / "zlib.dll").write_bytes(b"one")
        (self.llvm / "zlib.dll").write_bytes(b"two")
        with self.assertRaisesRegex(ValueError, "Conflicting DLL sources"):
            bundler.bundle(self.bin, [self.llvm, other], self.system, lambda _: [])

    def test_import_path_cannot_escape_binary_directory(self):
        for name in ("../outside.dll", "C:\\outside.dll", "sub/outside.dll"):
            with (
                self.subTest(name=name),
                self.assertRaisesRegex(ValueError, "Invalid DLL"),
            ):
                bundler.bundle(
                    self.bin, [self.llvm], self.system, lambda _, name=name: [name]
                )

    def test_dumpbin_normal_and_delay_imports_are_parsed(self):
        output = """Dump of file mux.exe
  Image has the following dependencies:
    KERNEL32.dll
    LLVM-C.dll
  Image has the following delay load dependencies:
    delayed.dll
  Summary
    1000 .rdata
"""
        with patch.object(
            subprocess,
            "run",
            return_value=subprocess.CompletedProcess([], 0, stdout=output, stderr=""),
        ):
            self.assertEqual(
                bundler.imported_dlls(Path("mux.exe")),
                ["KERNEL32.dll", "LLVM-C.dll", "delayed.dll"],
            )


if __name__ == "__main__":
    unittest.main()
