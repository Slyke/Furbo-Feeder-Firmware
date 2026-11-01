#!/usr/bin/env python3
"""Rebuild the compact, public-domain IANA TZif bundle. Requires zic."""
import argparse
import hashlib
import json
import pathlib
import re
import subprocess
import tarfile
import tempfile
import urllib.request

parser = argparse.ArgumentParser()
parser.add_argument("version", help="IANA release, e.g. 2026f")
args = parser.parse_args()
if not re.fullmatch(r"20[0-9]{2}[a-z]", args.version):
    parser.error("invalid IANA version")
repo = pathlib.Path(__file__).resolve().parent.parent
with tempfile.TemporaryDirectory(prefix="furbo-tzdb-") as work:
    work = pathlib.Path(work)
    archive = work / "tzdata.tar.gz"
    url = f"https://data.iana.org/time-zones/releases/tzdata{args.version}.tar.gz"
    urllib.request.urlretrieve(url, archive)
    source, compiled = work / "source", work / "compiled"
    source.mkdir()
    with tarfile.open(archive) as data:
        for member in data.getmembers():
            destination = (source / member.name).resolve()
            if not destination.is_relative_to(source.resolve()) or not member.isfile():
                continue
            with data.extractfile(member) as stream:
                destination.parent.mkdir(parents=True, exist_ok=True)
                destination.write_bytes(stream.read())
    regions = "africa antarctica asia australasia europe northamerica southamerica etcetera backward".split()
    subprocess.run(["zic", "-b", "fat", "-d", str(compiled), *[str(source / r) for r in regions]], check=True)
    blob, index, seen = bytearray(), {}, {}
    for file in sorted(compiled.rglob("*")):
        if not file.is_file():
            continue
        body = file.read_bytes()
        digest = hashlib.sha256(body).hexdigest()
        if digest not in seen:
            seen[digest] = [len(blob), len(body)]
            blob.extend(body)
        index[file.relative_to(compiled).as_posix()] = seen[digest]
    target = repo / "tzdb"
    target.mkdir(exist_ok=True)
    (target / "zones.bin").write_bytes(blob)
    (target / "index.json").write_text(json.dumps(index, separators=(",", ":")) + "\n")
    (target / "VERSION").write_text(args.version + "\n")
    (target / "LICENSE").write_bytes((source / "LICENSE").read_bytes())
    (target / "SOURCE.json").write_text(json.dumps({"url": url, "archive_sha256": hashlib.sha256(archive.read_bytes()).hexdigest()}, indent=2) + "\n")
print(f"Bundled {len(index)} timezone names; {len(blob)} bytes")
