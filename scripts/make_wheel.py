#!/usr/bin/env python3
"""Write one platform wheel for lanekeep.

Driven by ``scripts/build-python-wheels.sh``; see that file for why the distribution has
this shape at all. This module's job is narrow: given a binary and a platform tag, produce
a valid, reproducible ``.whl``.

A wheel is a zip with a specific table of contents, and the parts that matter here are:

``lanekeep-<version>.data/scripts/<binary>``
    The only location whose contents an installer places onto ``PATH``, executable.

``lanekeep-<version>.dist-info/METADATA``
    What PyPI renders on the project page.

``lanekeep-<version>.dist-info/WHEEL``
    Declares the format version and the compatibility tag. ``Root-Is-Purelib: false`` is
    the load-bearing line — it says this wheel is platform-specific.

``lanekeep-<version>.dist-info/RECORD``
    A checksum and size for every other entry. pip verifies it on install, so a wrong
    hash here is an install failure rather than something that degrades quietly.

Nothing importable is shipped. There is no package directory, no ``__init__.py`` and no
console-script shim: ``pip install lanekeep`` exists to put a binary on ``PATH``, and a
Python module that only re-execs it would be a second thing to keep in step with the first.
"""

from __future__ import annotations

import argparse
import base64
import csv
import hashlib
import io
import re
import stat
import sys
import zipfile
from pathlib import Path

# The zip epoch. Any fixed value would do; this one is the earliest a zip can express, and
# using the same one every time is what makes two builds of the same input byte-identical.
EPOCH = (1980, 1, 1, 0, 0, 0)

SUMMARY = "Deterministic, AST-based architectural conformance checking"


def workspace_field(cargo_toml: str, field: str) -> str:
    """Read one scalar from ``[workspace.package]``.

    Parsed with a regex rather than a TOML library on purpose: this runs on whatever
    Python the runner happens to have, ``tomllib`` only arrived in 3.11, and taking a
    dependency to read six well-known lines would be the larger risk.
    """
    section = cargo_toml.split("[workspace.package]", 1)[1].split("\n[", 1)[0]
    match = re.search(rf'^{field}\s*=\s*"([^"]+)"', section, re.MULTILINE)
    if not match:
        raise SystemExit(f"error: no {field} in [workspace.package]")
    return match.group(1)


def workspace_list(cargo_toml: str, field: str) -> list[str]:
    section = cargo_toml.split("[workspace.package]", 1)[1].split("\n[", 1)[0]
    match = re.search(rf"^{field}\s*=\s*\[([^\]]*)\]", section, re.MULTILINE)
    if not match:
        return []
    return re.findall(r'"([^"]+)"', match.group(1))


def metadata(version: str, repo_root: Path, readme: Path) -> str:
    cargo_toml = (repo_root / "Cargo.toml").read_text(encoding="utf-8")

    license_expression = workspace_field(cargo_toml, "license")
    homepage = workspace_field(cargo_toml, "homepage")
    repository = workspace_field(cargo_toml, "repository")
    authors = workspace_list(cargo_toml, "authors")
    keywords = workspace_list(cargo_toml, "keywords")

    lines = [
        # 2.1 rather than anything newer. The later revisions bring `License-Expression`,
        # which is a better way to say `MIT OR Apache-2.0` than two classifiers — but an
        # installer that predates the field rejects the whole wheel, and the gain is
        # cosmetic. This is the floor every installer in use understands.
        "Metadata-Version: 2.1",
        "Name: lanekeep",
        f"Version: {version}",
        f"Summary: {SUMMARY}",
        f"License: {license_expression}",
        f"Project-URL: Homepage, {homepage}",
        f"Project-URL: Repository, {repository}",
        f"Project-URL: Changelog, {repository}/blob/main/CHANGELOG.md",
    ]

    for author in authors:
        # `Name <email>` splits into the two fields PyPI displays separately.
        parsed = re.match(r"^(.*?)\s*<(.+)>$", author)
        if parsed:
            lines.append(f"Author: {parsed.group(1)}")
            lines.append(f"Author-email: {parsed.group(2)}")
        else:
            lines.append(f"Author: {author}")

    lines += [
        "Classifier: Development Status :: 4 - Beta",
        "Classifier: Intended Audience :: Developers",
        "Classifier: License :: OSI Approved :: MIT License",
        "Classifier: License :: OSI Approved :: Apache Software License",
        "Classifier: Programming Language :: Rust",
        "Classifier: Topic :: Software Development :: Quality Assurance",
    ]
    if keywords:
        lines.append(f"Keywords: {','.join(keywords)}")

    # No `Requires-Dist` of any kind, and that is the point: the binary carries its own
    # JavaScript engine, so installing lanekeep pulls in nothing else.
    #
    # 3.8 is not a claim about what lanekeep runs on — it runs on nothing, being a binary.
    # It is the oldest interpreter whose installer reliably understands these tags.
    lines += [
        "Requires-Python: >=3.8",
        "Description-Content-Type: text/markdown",
        "",
        readme.read_text(encoding="utf-8"),
    ]
    return "\n".join(lines)


def wheel_metadata(tag: str) -> str:
    return "\n".join(
        [
            "Wheel-Version: 1.0",
            "Generator: lanekeep build-python-wheels.sh",
            # False, because this wheel carries a platform-specific binary. A true here
            # would tell the installer the contents are portable Python.
            "Root-Is-Purelib: false",
            f"Tag: py3-none-{tag}",
            "",
        ]
    )


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--version", required=True)
    parser.add_argument("--tag", required=True, help="wheel platform tag")
    parser.add_argument("--binary", required=True, type=Path)
    parser.add_argument("--binary-name", required=True)
    parser.add_argument("--readme", required=True, type=Path)
    parser.add_argument("--license", action="append", default=[], type=Path)
    parser.add_argument("--out", required=True, type=Path)
    args = parser.parse_args()

    repo_root = Path(__file__).resolve().parent.parent

    dist_info = f"lanekeep-{args.version}.dist-info"
    data = f"lanekeep-{args.version}.data"

    # (archive path, bytes, mode). Order here is the order in the zip; RECORD is appended
    # last because it has to hash everything before it.
    entries: list[tuple[str, bytes, int]] = [
        (
            f"{data}/scripts/{args.binary_name}",
            args.binary.read_bytes(),
            # 0o755 written explicitly rather than copied from the file on disk. What
            # arrives here is 0644: `actions/upload-artifact` zips its input and
            # `download-artifact` unpacks it without permissions, so the mode of the
            # source file says nothing about what the mode should be. The npm lane learned
            # this by shipping a binary nobody could execute.
            0o755,
        ),
        (
            f"{dist_info}/METADATA",
            metadata(args.version, repo_root, args.readme).encode("utf-8"),
            0o644,
        ),
        (f"{dist_info}/WHEEL", wheel_metadata(args.tag).encode("utf-8"), 0o644),
    ]

    # Both licenses travel with the wheel, for the same reason both travel in the tarball:
    # someone who installed from an index has no other copy of either.
    for path in args.license:
        entries.append((f"{dist_info}/{path.name}", path.read_bytes(), 0o644))

    record = io.StringIO()
    # `lineterminator` set explicitly: csv defaults to CRLF, and RECORD is read as text.
    writer = csv.writer(record, lineterminator="\n")
    for name, payload, _ in entries:
        digest = hashlib.sha256(payload).digest()
        encoded = base64.urlsafe_b64encode(digest).rstrip(b"=").decode("ascii")
        writer.writerow([name, f"sha256={encoded}", len(payload)])
    # RECORD lists itself with neither, because it cannot contain its own hash.
    writer.writerow([f"{dist_info}/RECORD", "", ""])
    entries.append((f"{dist_info}/RECORD", record.getvalue().encode("utf-8"), 0o644))

    args.out.mkdir(parents=True, exist_ok=True)
    destination = args.out / f"lanekeep-{args.version}-py3-none-{args.tag}.whl"

    with zipfile.ZipFile(destination, "w", zipfile.ZIP_DEFLATED) as archive:
        for name, payload, mode in entries:
            info = zipfile.ZipInfo(name, date_time=EPOCH)
            # A zip stores permissions separately from the filesystem, in the top 16 bits
            # of `external_attr`. Without the file-type bit an installer sees mode 0 and
            # falls back to something conservative, which for the binary means not
            # executable.
            info.external_attr = (stat.S_IFREG | mode) << 16
            info.compress_type = zipfile.ZIP_DEFLATED
            archive.writestr(info, payload)

    print(f"  {destination.name}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
