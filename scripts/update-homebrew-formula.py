#!/usr/bin/env python3
"""Pin packaging/homebrew/kborg.rb to a released tag.

Downloads GitHub's source tarball for the tag, computes its SHA-256 and
rewrites the formula's url, sha256 and version-dependent test. The tag must
already be pushed and the repository readable without credentials (public).

Usage: update-homebrew-formula.py 0.4.0 [--formula PATH] [--repo OWNER/NAME]
"""
import argparse
import hashlib
import re
import sys
import urllib.request
from pathlib import Path


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("version", help="release version without the leading v, e.g. 0.4.0")
    parser.add_argument("--repo", default="c0dewars/kborg")
    parser.add_argument(
        "--formula",
        type=Path,
        default=Path(__file__).resolve().parent.parent / "packaging/homebrew/kborg.rb",
    )
    args = parser.parse_args()
    if not re.fullmatch(r"\d+\.\d+\.\d+", args.version):
        sys.exit("version must look like 0.4.0")
    url = f"https://github.com/{args.repo}/archive/refs/tags/v{args.version}.tar.gz"
    digest = hashlib.sha256()
    with urllib.request.urlopen(url, timeout=60) as response:
        for chunk in iter(lambda: response.read(1 << 16), b""):
            digest.update(chunk)
    sha256 = digest.hexdigest()
    text = args.formula.read_text()
    text = re.sub(r'^  url ".*"$', f'  url "{url}"', text, count=1, flags=re.M)
    text = re.sub(r'^  sha256 ".*"$', f'  sha256 "{sha256}"', text, count=1, flags=re.M)
    args.formula.write_text(text)
    print(f"{args.formula}: v{args.version} sha256 {sha256}")


if __name__ == "__main__":
    main()
