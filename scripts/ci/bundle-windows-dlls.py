#!/usr/bin/env python3
"""Bundle the transitive non-system DLL imports of a Windows installation."""

import argparse
from collections import deque
import hashlib
import os
from pathlib import Path
import re
import shutil
import subprocess


def imported_dlls(binary, inspector="dumpbin.exe"):
    result = subprocess.run(
        [inspector, "/nologo", "/dependents", str(binary)],
        capture_output=True, text=True, check=True, timeout=60,
    )
    return [line.strip() for line in result.stdout.splitlines()
            if re.fullmatch(r"[^\s/\\:]+\.dll", line.strip(), re.IGNORECASE)]


def dll_index(directory):
    result = {}
    for path in sorted(directory.iterdir()):
        if path.is_file() and path.suffix.lower() == ".dll":
            name = path.name.casefold()
            if name in result:
                raise ValueError(f"Duplicate DLL name in {directory}: {path.name}")
            result[name] = path
    return result


def digest(path):
    with path.open("rb") as source:
        return hashlib.file_digest(source, "sha256").digest()


def bundle(binary_directory, search_directories, system_directory, inspect=imported_dlls):
    sources = {}
    for directory in search_directories:
        for name, path in dll_index(directory).items():
            previous = sources.get(name)
            if previous is not None and digest(previous) != digest(path):
                raise ValueError(f"Conflicting DLL sources: {previous} and {path}")
            sources[name] = path
    system = dll_index(system_directory)
    installed = dll_index(binary_directory)
    pending = deque(sorted(path for path in binary_directory.iterdir()
                           if path.is_file() and path.suffix.lower() in (".dll", ".exe")))
    if not pending:
        raise ValueError(f"No Windows executables or DLLs in {binary_directory}")
    inspected = set()
    copied = []
    while pending:
        binary = pending.popleft()
        if binary.name.casefold() in inspected:
            continue
        inspected.add(binary.name.casefold())
        for dependency in inspect(binary):
            if Path(dependency).name != dependency or any(c in dependency for c in "/\\:"):
                raise ValueError(f"Invalid DLL import in {binary}: {dependency}")
            name = dependency.casefold()
            # Windows resolves API-set contract names without a physical DLL.
            if name.startswith(("api-ms-", "ext-ms-")):
                continue
            if name in installed:
                pending.append(installed[name])
                continue
            source = sources.get(name)
            if source is None:
                if name in system:
                    continue
                raise FileNotFoundError(f"Missing DLL {dependency}, imported by {binary}")
            destination = binary_directory / source.name
            shutil.copy2(source, destination)
            installed[name] = destination
            pending.append(destination)
            copied.append(destination.name)
            print(f"Bundled {destination.name} (imported by {binary.name})")
    return copied


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--bin-dir", type=Path, required=True)
    parser.add_argument("--search-dir", type=Path, action="append", required=True)
    parser.add_argument("--system-dir", type=Path)
    parser.add_argument("--inspector", default="dumpbin.exe")
    options = parser.parse_args()
    system = options.system_dir
    if system is None:
        system = Path(os.environ["SystemRoot"]) / "System32"
    bundle(options.bin_dir, options.search_dir, system,
           lambda path: imported_dlls(path, options.inspector))


if __name__ == "__main__":
    main()
