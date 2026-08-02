#!/usr/bin/env python3
"""Fail if a Linux binary needs a newer glibc than it is about to claim.

    check_glibc_floor.py <binary> <max-version>

A dynamically linked ELF records, in ``.gnu.version_r``, the symbol versions it needs from
each shared library it loads. The highest ``GLIBC_x.y`` among them is the oldest glibc the
binary can run against: glibc is backward compatible, so a binary built against 2.17 runs on
2.39, but a binary needing 2.39 does not start on 2.35.

# Why this is a gate and not a comment

Nothing in a release states the floor, so it is inherited from whatever image the runner
label happens to point at. When ``ubuntu-latest`` rolled from 22.04 to 24.04, lanekeep's
floor moved 2.35 → 2.39 with it, and v0.3.1 shipped a Linux binary that will not start on
Ubuntu 22.04, Debian 12 or RHEL 9. Nothing failed. The build was green, the smoke test
passed — it ran on the machine that built it, which is the one machine where the floor is
never wrong.

A wheel is the first artifact that has to *name* the floor, because the manylinux tag in its
filename is exactly that claim. Overstating it is worse than shipping nothing: the install
succeeds and the failure arrives later, as a linker error naming a library the user did not
know they had. So the claim is checked against the binary rather than trusted.

The same binary goes to npm, to the releases page and to Homebrew, so checking it here
protects those too — this is simply the lane where the number is written down.
"""

from __future__ import annotations

import struct
import sys


def sections(data: bytes) -> tuple[list[dict], int]:
    if data[:4] != b"\x7fELF":
        raise SystemExit("error: not an ELF file")
    if data[4] != 2:
        raise SystemExit("error: not a 64-bit ELF")

    (shoff,) = struct.unpack_from("<Q", data, 0x28)
    entsize, count, strndx = struct.unpack_from("<HHH", data, 0x3A)

    found = []
    for index in range(count):
        offset = shoff + index * entsize
        name, _type, _flags, _addr, off, size, link, info, _align, _es = struct.unpack_from(
            "<IIQQQQIIQQ", data, offset
        )
        found.append({"name": name, "off": off, "size": size, "link": link, "info": info})
    return found, strndx


def read_string(data: bytes, base: int, offset: int) -> str:
    start = base + offset
    return data[start : data.index(b"\0", start)].decode()


def version_key(version: str) -> tuple[int, ...]:
    parts = version.split("_")[-1].split(".")
    if not all(part.isdigit() for part in parts):
        return (0,)
    return tuple(int(part) for part in parts)


def glibc_floor(path: str) -> tuple[tuple[int, ...], dict[str, list[str]]]:
    data = open(path, "rb").read()
    found, strndx = sections(data)
    shstr = found[strndx]

    def section_name(section: dict) -> str:
        return read_string(data, shstr["off"], section["name"])

    verneed = next((s for s in found if section_name(s) == ".gnu.version_r"), None)
    if verneed is None:
        # A fully static binary needs no glibc at all, which is a floor of zero rather than
        # an error.
        return (0,), {}

    strtab = found[verneed["link"]]
    required: dict[str, list[str]] = {}
    floor = (0,)

    offset = verneed["off"]
    for _ in range(verneed["info"]):
        _version, count, file_offset, aux, next_offset = struct.unpack_from(
            "<HHIII", data, offset
        )
        library = read_string(data, strtab["off"], file_offset)
        cursor = offset + aux
        for _ in range(count):
            _hash, _flags, _other, name_offset, next_aux = struct.unpack_from(
                "<IHHII", data, cursor
            )
            name = read_string(data, strtab["off"], name_offset)
            required.setdefault(library, []).append(name)
            if name.startswith("GLIBC_"):
                floor = max(floor, version_key(name))
            if not next_aux:
                break
            cursor += next_aux
        if not next_offset:
            break
        offset += next_offset

    return floor, required


def main() -> int:
    if len(sys.argv) != 3:
        raise SystemExit("usage: check_glibc_floor.py <binary> <max-version>")

    path, ceiling_text = sys.argv[1], sys.argv[2]
    ceiling = version_key(ceiling_text)
    floor, required = glibc_floor(path)

    rendered = ".".join(str(part) for part in floor)
    if floor > ceiling:
        print(f"error: {path} needs glibc {rendered}, but is about to claim {ceiling_text}", file=sys.stderr)
        print("", file=sys.stderr)
        # The offending symbols, because "needs 2.39" is not actionable on its own and the
        # answer is usually one or two symbols from a single library.
        for library, versions in sorted(required.items()):
            over = sorted({v for v in versions if version_key(v) > ceiling}, key=version_key)
            if over:
                print(f"       {library} requires {', '.join(over)}", file=sys.stderr)
        print("", file=sys.stderr)
        print("       the release build pins this floor with a versioned target triple —", file=sys.stderr)
        print("       see the `--target ...-gnu.2.17` in .github/workflows/release.yml", file=sys.stderr)
        return 1

    print(f"  glibc floor {rendered} (claiming {ceiling_text})")
    return 0


if __name__ == "__main__":
    sys.exit(main())
