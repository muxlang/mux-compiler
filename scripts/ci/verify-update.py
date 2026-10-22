#!/usr/bin/env python3
"""Exercise the real packaged updater against a loopback release server."""

import functools
import hashlib
import http.server
import json
import os
from pathlib import Path
import platform
import shutil
import subprocess
import sys
import tempfile
import tarfile
import threading
import zipfile


def run(mux, *args, env=None, succeeds=True):
    result = subprocess.run(
        [str(mux), *args], env=env, capture_output=True, text=True, timeout=120
    )
    print(result.stdout, end="")
    print(result.stderr, end="", file=sys.stderr)
    if (result.returncode == 0) != succeeds:
        raise AssertionError(f"unexpected exit {result.returncode}: {args}")
    return result.stdout


def smoke_shell(windows):
    if not windows:
        return "bash"

    # The verifier runs under Git Bash in Windows CI. A child process that
    # searches PATH for `bash` can instead select Windows' WSL launcher, which
    # fails on runners without a Linux distribution. Reuse the parent shell
    # when available, converting its MSYS path to a native Windows path.
    bash = os.environ.get("BASH")
    if bash:
        candidate = Path(bash)
        if candidate.is_file():
            return str(candidate)

        cygpath = shutil.which("cygpath")
        if cygpath:
            result = subprocess.run(
                [cygpath, "-w", bash], capture_output=True, text=True, check=True
            )
            candidate = Path(result.stdout.strip())
            if candidate.is_file():
                return str(candidate)

    # Git for Windows commonly puts git.exe in <Git>/cmd and Bash in
    # <Git>/bin. This fallback also works when BASH is not exported.
    git = shutil.which("git")
    if git:
        git_dir = Path(git).resolve().parent
        for root in (git_dir.parent, git_dir.parent.parent):
            candidate = root / "bin" / "bash.exe"
            if candidate.is_file():
                return str(candidate)

    raise RuntimeError("could not locate Git for Windows Bash for the smoke test")


def main():
    source_mux = Path(sys.argv[1]).resolve()
    windows = platform.system() == "Windows"
    system = {"Windows": "windows", "Darwin": "macos", "Linux": "linux"}[platform.system()]
    arch = "aarch64" if platform.machine().lower() in ("arm64", "aarch64") else "x86_64"
    target = f"{system}-{arch}"
    version = run(source_mux, "__version").strip()
    with tempfile.TemporaryDirectory(prefix="mux-update-smoke-") as temporary:
        root = Path(temporary)
        bundle = root / f"mux-{target}"
        shutil.copytree(source_mux.parent, bundle / "bin")
        shutil.copytree(source_mux.parent.parent / "lib", bundle / "lib")
        # Installation state belongs to the receipt, never to release archives.
        for state in (bundle / "bin").glob(".mux-*"):
            if state.is_dir():
                shutil.rmtree(state)
            else:
                state.unlink()
        install = root / "installation with spaces"
        shutil.copytree(bundle / "bin", install / "bin")
        # Deliberately separate libraries from bin's parent to test the receipt.
        lib = root / "custom libraries"
        shutil.copytree(bundle / "lib", lib)
        mux = install / "bin" / ("mux.exe" if windows else "mux")
        run(mux, "__register-install", "--bundle-dir", str(bundle),
            "--bin-dir", str(mux.parent), "--lib-dir", str(lib), "--target", target)
        receipt_path = mux.parent / ".mux-install.json"
        receipt = json.loads(receipt_path.read_text())
        receipt["version"] = "0.0.0"
        obsolete = lib / "obsolete-runtime-library"
        obsolete.write_text("owned by the previous release")
        receipt["files"].append({"area": "lib", "name": obsolete.name})
        receipt_path.write_text(json.dumps(receipt))
        unrelated = mux.parent / "user-notes.txt"
        unrelated.write_text("preserve this file")
        served = root / "served"
        served.mkdir()
        extension = "zip" if windows else "tar.gz"
        archive = served / f"mux-{target}.{extension}"
        # These fixtures contain debug binaries. Favor test time over size.
        if windows:
            with zipfile.ZipFile(archive, "w", zipfile.ZIP_DEFLATED, compresslevel=1) as packed:
                for entry in sorted(bundle.rglob("*")):
                    packed.write(entry, entry.relative_to(root).as_posix())
        else:
            with tarfile.open(archive, "w:gz", compresslevel=1) as packed:
                packed.add(bundle, arcname=bundle.name)
        checksum = Path(str(archive) + ".sha256")
        digest = hashlib.sha256(archive.read_bytes()).hexdigest()
        checksum.write_text(f"{digest}  {archive.name}\n")
        handler = functools.partial(http.server.SimpleHTTPRequestHandler, directory=str(served))
        server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), handler)
        server.daemon_threads = True
        base = f"http://127.0.0.1:{server.server_port}"
        metadata = {
            "tag_name": f"v{version}", "prerelease": False, "draft": False,
            "assets": [{"name": name, "browser_download_url": f"{base}/{name}"}
                       for name in (archive.name, checksum.name)],
        }
        (served / "latest").write_text(json.dumps(metadata))
        thread = threading.Thread(target=server.serve_forever, daemon=True)
        thread.start()
        environment = dict(os.environ, MUX_UPDATE_API_URL=f"{base}/latest")
        environment.pop("MUX_RUNTIME_LIB", None)
        try:
            before = receipt_path.read_bytes()
            run(mux, "update", "--check", env=environment)
            assert receipt_path.read_bytes() == before, "--check modified receipt"
            checksum.write_text(f"{'0' * 64}  {archive.name}\n")
            run(mux, "update", env=environment, succeeds=False)
            assert receipt_path.read_bytes() == before, "checksum failure modified receipt"
            checksum.write_text(f"{digest}  {archive.name}\n")
            output = run(mux, "update", env=environment)
            assert "Updated Mux" in output, "final success missing when update returned"
            assert json.loads(receipt_path.read_text())["version"] == version
            assert unrelated.read_text() == "preserve this file"
            assert not obsolete.exists(), "obsolete owned library survived update"
            assert run(mux, "__version").strip() == version
            run(mux, "update", env=environment)
            assert json.loads(receipt_path.read_text())["version"] == version
            installed_receipt = receipt_path.read_bytes()
            future = json.loads(installed_receipt)
            future["version"] = "9999.0.0"
            receipt_path.write_text(json.dumps(future))
            future_receipt = receipt_path.read_bytes()
            compiler_digest = hashlib.sha256(mux.read_bytes()).digest()
            def installed_files():
                return {str(path): (path.stat().st_size, path.stat().st_mtime_ns)
                        for directory in (mux.parent, lib)
                        for path in directory.iterdir() if path.is_file()}
            future_files = installed_files()
            run(mux, "update", env=environment)
            assert receipt_path.read_bytes() == future_receipt, "newer installation changed"
            assert installed_files() == future_files, "newer installation files changed"
            assert hashlib.sha256(mux.read_bytes()).digest() == compiler_digest, "compiler downgraded"
            # Restore the old receipt so error cases take the update path.
            receipt_path.write_bytes(before)
            missing_asset = dict(metadata, assets=[])
            (served / "latest").write_text(json.dumps(missing_asset))
            run(mux, "update", env=environment, succeeds=False)
            assert receipt_path.read_bytes() == before, "missing asset modified receipt"
            assert hashlib.sha256(mux.read_bytes()).digest() == compiler_digest
            (served / "latest").write_text("invalid release metadata")
            run(mux, "update", env=environment, succeeds=False)
            assert receipt_path.read_bytes() == before, "invalid metadata modified receipt"
            assert hashlib.sha256(mux.read_bytes()).digest() == compiler_digest
            (served / "latest").write_text(json.dumps(metadata))
            receipt_path.write_bytes(installed_receipt)
            subprocess.run(
                [smoke_shell(windows), "scripts/ci/smoke-run.sh", str(mux)],
                env=environment, check=True, timeout=300,
            )
        finally:
            server.shutdown()
            server.server_close()
            thread.join()
    print(f"Packaged update passed: {target} ({extension})")


if __name__ == "__main__":
    main()
