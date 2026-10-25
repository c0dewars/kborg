#!/usr/bin/env python3
"""Build a Debian package (.deb) from an already built Linux KBorg binary.

  python3 scripts/package-linux.py --binary target/x86_64-unknown-linux-gnu/release/kborg \\
      --version 0.5.0-rc.1 --arch amd64 [--out dist/packages]

Package contents (no man page):
  /usr/bin/kborg
  /usr/share/bash-completion/completions/kborg
  /usr/share/zsh/vendor-completions/_kborg
  /usr/share/fish/vendor_completions.d/kborg.fish
  /usr/share/doc/kborg/{copyright,LICENSE,THIRD_PARTY_NOTICES.md}

Shell completions are generated with `kborg completions SHELL`. When the target
binary cannot run on this machine (a cross build), pass --completions-binary
with a host build of the same source revision.

Uses `dpkg-deb --root-owner-group --build` when available; otherwise the .deb
(an ar archive of debian-binary, control.tar.gz and data.tar.gz) is written
directly with the standard library. Runtime Depends are derived from the ELF
DT_NEEDED entries and the highest GLIBC_x.y symbol version the binary uses.

The RPM is built from packaging/rpm/kborg.spec instead; see docs/OPERATIONS.md.
Standard library only; Python 3.9+.
"""
import argparse
import gzip
import hashlib
import io
import os
from pathlib import Path
import re
import shutil
import struct
import subprocess
import sys
import tarfile
import tempfile
import time

REPO = Path(__file__).resolve().parent.parent
ARCHES = {"amd64": 62, "arm64": 183}  # Debian arch -> ELF e_machine
MAINTAINER = "KBorg maintainers <https://github.com/c0dewars/kborg/issues>"
HOMEPAGE = "https://github.com/c0dewars/kborg"
# Shared libraries a Rust binary may link, mapped to the Debian package
# providing them. Anything else is an error: the Depends line would be wrong.
SONAMES = {"libc.so.6": "libc6", "libm.so.6": "libc6", "libpthread.so.0": "libc6",
           "libdl.so.2": "libc6", "librt.so.1": "libc6", "libutil.so.1": "libc6",
           "ld-linux-x86-64.so.2": "libc6", "ld-linux-aarch64.so.1": "libc6",
           "libgcc_s.so.1": "libgcc-s1"}


def debian_version(version):
    """0.5.0-rc.1 -> 0.5.0~rc.1 so pre-releases sort before the release."""
    if not re.fullmatch(r"[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.]+)?", version):
        sys.exit(f"unsupported version {version!r}; expected MAJOR.MINOR.PATCH[-PRERELEASE]")
    return version.replace("-", "~", 1)


def elf_dependencies(path, arch):
    """(needed sonames, highest GLIBC version tuple) from a 64-bit LE ELF."""
    data = Path(path).read_bytes()
    if data[:4] != b"\x7fELF" or data[4] != 2 or data[5] != 1:
        sys.exit(f"{path}: not a 64-bit little-endian ELF binary (is this a Linux build?)")
    machine = struct.unpack_from("<H", data, 18)[0]
    if machine != ARCHES[arch]:
        sys.exit(f"{path}: ELF machine {machine} does not match --arch {arch}")
    shoff, = struct.unpack_from("<Q", data, 40)
    shentsize, shnum = struct.unpack_from("<HH", data, 58)
    sections = []
    for index in range(shnum):
        _, kind, _, _, offset, size, link, _, _, entsize = struct.unpack_from(
            "<IIQQQQIIQQ", data, shoff + index * shentsize)
        sections.append((kind, offset, size, link, entsize))
    needed, glibc = [], (0, 0)
    for kind, offset, size, link, _ in sections:
        if kind != 6:  # SHT_DYNAMIC
            continue
        strtab_offset = sections[link][1]
        for entry in range(0, size, 16):
            tag, value = struct.unpack_from("<qQ", data, offset + entry)
            if tag == 0:
                break
            if tag == 1:  # DT_NEEDED
                end = data.index(b"\0", strtab_offset + value)
                needed.append(data[strtab_offset + value:end].decode())
        strtab = data[strtab_offset:strtab_offset + sections[link][2]]
        for major, minor in re.findall(rb"\bGLIBC_([0-9]+)\.([0-9]+)", strtab):
            glibc = max(glibc, (int(major), int(minor)))
    return needed, glibc


def depends(needed, glibc):
    unknown = sorted(set(needed) - set(SONAMES))
    if unknown:
        sys.exit(f"binary links unmapped shared libraries {unknown}; extend SONAMES in {Path(__file__).name}")
    packages = sorted({SONAMES[name] for name in needed})
    return ", ".join(f"libc6 (>= {glibc[0]}.{glibc[1]})" if name == "libc6" and glibc != (0, 0)
                     else name for name in packages)


def completions(runner):
    result = {}
    for shell in ("bash", "zsh", "fish"):
        text = subprocess.run([str(runner), "completions", shell], check=True,
                              capture_output=True, timeout=30).stdout
        if b"kborg" not in text:
            sys.exit(f"`kborg completions {shell}` produced no kborg completion")
        result[shell] = text
    return result


def stage_tree(root, binary, shells):
    files = {
        "usr/bin/kborg": (Path(binary).read_bytes(), 0o755),
        "usr/share/bash-completion/completions/kborg": (shells["bash"], 0o644),
        "usr/share/zsh/vendor-completions/_kborg": (shells["zsh"], 0o644),
        "usr/share/fish/vendor_completions.d/kborg.fish": (shells["fish"], 0o644),
        "usr/share/doc/kborg/LICENSE": ((REPO / "LICENSE").read_bytes(), 0o644),
        "usr/share/doc/kborg/THIRD_PARTY_NOTICES.md": ((REPO / "THIRD_PARTY_NOTICES.md").read_bytes(), 0o644),
        "usr/share/doc/kborg/copyright": ((
            "Format: https://www.debian.org/doc/packaging-manuals/copyright-format/1.0/\n"
            "Upstream-Name: kborg\n"
            f"Source: {HOMEPAGE}\n\n"
            "Files: *\n"
            "Copyright: KBorg contributors\n"
            "License: Apache-2.0\n"
            " The full license text is in /usr/share/doc/kborg/LICENSE.\n"
            " Bundled third-party notices: /usr/share/doc/kborg/THIRD_PARTY_NOTICES.md.\n").encode(), 0o644),
    }
    for relative, (content, mode) in files.items():
        path = root / relative
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_bytes(content)
        path.chmod(mode)
    for directory in [p for p in root.rglob("*") if p.is_dir()] + [root]:
        directory.chmod(0o755)
    return files


def control_text(version, arch, installed_kib, depends_line):
    return (f"Package: kborg\n"
            f"Version: {version}\n"
            f"Architecture: {arch}\n"
            f"Maintainer: {MAINTAINER}\n"
            f"Installed-Size: {installed_kib}\n"
            f"Depends: {depends_line}\n"
            f"Section: admin\n"
            f"Priority: optional\n"
            f"Homepage: {HOMEPAGE}\n"
            f"Description: Kubernetes operations, under your control\n"
            f" KBorg is a fast Kubernetes terminal UI with k9s-compatible keys, a\n"
            f" multi-cluster dashboard and a read-only-by-default safety model:\n"
            f" every change gets a server dry run, a readable diff and typed\n"
            f" confirmation.\n")


def _tar(entries, mtime):
    """Deterministic gzip'd tar of (name, bytes|None for dir, mode), root:root."""
    raw = io.BytesIO()
    with tarfile.open(fileobj=raw, mode="w", format=tarfile.GNU_FORMAT) as archive:
        for name, content, mode in entries:
            info = tarfile.TarInfo(name)
            info.uid = info.gid = 0
            info.uname = info.gname = "root"
            info.mtime, info.mode = mtime, mode
            if content is None:
                info.type = tarfile.DIRTYPE
                archive.addfile(info)
            else:
                info.size = len(content)
                archive.addfile(info, io.BytesIO(content))
    out = io.BytesIO()
    with gzip.GzipFile(fileobj=out, mode="wb", compresslevel=9, mtime=mtime) as zipped:
        zipped.write(raw.getvalue())
    return out.getvalue()


def _ar(members, mtime):
    out = bytearray(b"!<arch>\n")
    for name, content in members:
        header = (f"{name:<16}{mtime:<12}{0:<6}{0:<6}{'100644':<8}{len(content):<10}`\n").encode()
        assert len(header) == 60
        out += header + content
        if len(content) % 2:
            out += b"\n"
    return bytes(out)


def build_python(root, control, md5sums, output, mtime):
    data_entries = [("./", None, 0o755)]
    for path in sorted(root.rglob("*")):
        relative = "./" + path.relative_to(root).as_posix()
        if path.is_dir():
            data_entries.append((relative + "/", None, 0o755))
        else:
            data_entries.append((relative, path.read_bytes(), path.stat().st_mode & 0o777))
    control_tar = _tar([("./", None, 0o755), ("./control", control.encode(), 0o644),
                        ("./md5sums", md5sums.encode(), 0o644)], mtime)
    output.write_bytes(_ar([("debian-binary", b"2.0\n"), ("control.tar.gz", control_tar),
                            ("data.tar.gz", _tar(data_entries, mtime))], mtime))


def build_dpkg(root, control, md5sums, output):
    debian = root / "DEBIAN"
    debian.mkdir(mode=0o755)
    (debian / "control").write_text(control)
    (debian / "md5sums").write_text(md5sums)
    subprocess.run(["dpkg-deb", "--root-owner-group", "-Zgzip", "--build", str(root), str(output)],
                   check=True)


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--binary", required=True, type=Path, help="built Linux kborg binary")
    parser.add_argument("--version", required=True, help="Cargo version, e.g. 0.5.0 or 0.5.0-rc.1")
    parser.add_argument("--arch", required=True, choices=sorted(ARCHES))
    parser.add_argument("--out", type=Path, default=REPO / "dist/packages")
    parser.add_argument("--completions-binary", type=Path,
                        help="host-runnable kborg used for `kborg completions` (default: --binary)")
    parser.add_argument("--builder", choices=("auto", "dpkg-deb", "python"), default="auto")
    args = parser.parse_args()

    binary = args.binary.resolve(strict=True)
    version = debian_version(args.version)
    needed, glibc = elf_dependencies(binary, args.arch)
    depends_line = depends(needed, glibc)
    shells = completions((args.completions_binary or binary).resolve(strict=True))
    builder = args.builder
    if builder == "auto":
        builder = "dpkg-deb" if shutil.which("dpkg-deb") else "python"
    mtime = int(os.environ.get("SOURCE_DATE_EPOCH") or time.time())

    args.out.mkdir(parents=True, exist_ok=True)
    output = args.out.resolve() / f"kborg_{version}_{args.arch}.deb"
    with tempfile.TemporaryDirectory(prefix="kborg-deb-") as tmp:
        root = Path(tmp) / "root"
        files = stage_tree(root, binary, shells)
        installed_kib = sum((len(content) + 1023) // 1024 for content, _ in files.values())
        control = control_text(version, args.arch, installed_kib, depends_line)
        md5sums = "".join(f"{hashlib.md5(content).hexdigest()}  {name}\n"
                          for name, (content, _) in sorted(files.items()))
        if builder == "dpkg-deb":
            build_dpkg(root, control, md5sums, output)
        else:
            build_python(root, control, md5sums, output, mtime)
    digest = hashlib.sha256(output.read_bytes()).hexdigest()
    print(f"{output} ({builder}; Depends: {depends_line})\nsha256 {digest}")


if __name__ == "__main__":
    main()
