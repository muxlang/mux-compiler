#!/usr/bin/env python3
"""Test installer rollout compatibility without network or a real compiler."""
import hashlib
import os
from pathlib import Path
import subprocess
import tarfile
import tempfile
import unittest


class InstallerTests(unittest.TestCase):
    def install(self, supports_update, registration_succeeds=True):
        with tempfile.TemporaryDirectory(prefix="mux-installer-test-") as temporary:
            root = Path(temporary)
            bundle = root / "mux-linux-x86_64"
            (bundle / "bin").mkdir(parents=True)
            (bundle / "lib").mkdir()
            (bundle / "lib" / "libmux_runtime.a").write_text("runtime")
            (bundle / "bin" / "compiler-support.dylib").write_text("support")
            compiler = bundle / "bin" / "mux"
            compiler.write_text(f'''#!/bin/sh
case "$1" in
  __version) exit {0 if supports_update else 2} ;;
  __register-install) echo registered > "$MUX_TEST_MARKER"; exit {0 if registration_succeeds else 1} ;;
  *) exit 0 ;;
esac
''')
            compiler.chmod(0o755)
            archive = root / "mux-linux-x86_64.tar.gz"
            with tarfile.open(archive, "w:gz") as packed:
                packed.add(bundle, arcname=bundle.name)
            checksum = Path(str(archive) + ".sha256")
            checksum.write_text(f"{hashlib.sha256(archive.read_bytes()).hexdigest()}  {archive.name}\n")
            mocks = root / "commands"
            mocks.mkdir()
            curl = mocks / "curl"
            curl.write_text('''#!/bin/sh
# Installer invokes curl -fsSL URL -o DESTINATION.
cp "$MUX_TEST_DOWNLOADS/${2##*/}" "$4"
''')
            curl.chmod(0o755)
            uname = mocks / "uname"
            uname.write_text('#!/bin/sh\ncase "$1" in -s) echo Linux;; -m) echo x86_64;; esac\n')
            uname.chmod(0o755)
            destination = root / "installed files"
            marker = root / "registration"
            environment = dict(os.environ, PATH=f"{mocks}:{os.environ['PATH']}",
                               MUX_TEST_DOWNLOADS=str(root), MUX_TEST_MARKER=str(marker),
                               MUX_INSTALL_DIR=str(destination / "bin"),
                               MUX_LIB_DIR=str(destination / "custom lib"))
            installer = Path(__file__).resolve().parents[1] / "install.sh"
            result = subprocess.run(["sh", str(installer)], env=environment,
                                    capture_output=True, text=True, timeout=10)
            self.assertEqual(result.returncode, 0 if registration_succeeds else 1, result.stderr)
            self.assertEqual(marker.exists(), supports_update)
            self.assertTrue((destination / "bin" / "compiler-support.dylib").is_file())
            self.assertEqual("does not support mux update" in result.stdout, not supports_update)

    def test_pre_update_release_still_installs(self):
        self.install(False)

    def test_update_capable_release_registers(self):
        self.install(True)

    def test_registration_failure_is_fatal(self):
        self.install(True, False)


if __name__ == "__main__":
    unittest.main()
