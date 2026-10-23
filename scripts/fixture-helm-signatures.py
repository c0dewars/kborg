#!/usr/bin/env python3
"""Disposable OpenPGP fixtures; never reads a personal GPG home or Docker config.

create(root, {"first": chart1, "second": chart2}, helm) returns flat public
artifacts and writes fixtures.json. GnuPG runs only in a named, labeled container
with one new private bind mount. All private signing material is removed before
return. The exported public keys are fixture trust roots, not production keys.
"""
from __future__ import annotations

import argparse
import gzip
import io
import json
import os
from pathlib import Path, PurePosixPath
import re
import secrets
import shutil
import socket
import subprocess
import tarfile
import tempfile

IMAGE = "rust:1.99.0-slim@sha256:2752b332db73fdbb7dc576f06c82ed1f312005784ef913d7e04a28f5f55dc581"
IDENTITY = "KBorg disposable signature fixture <fixture@invalid.example>"


def _run(argv, *, env, timeout=60):
    # Private temporary files bound memory even if a fixture tool is noisy.
    with tempfile.TemporaryFile() as stdout, tempfile.TemporaryFile() as stderr:
        result = subprocess.run([str(arg) for arg in argv], stdin=subprocess.DEVNULL,
                                stdout=stdout, stderr=stderr, env=env, timeout=timeout)
        if stdout.tell() > 8 * 1024 * 1024 or stderr.tell() > 8 * 1024 * 1024:
            raise RuntimeError("fixture tool exceeded its diagnostic budget")
        stdout.seek(0)
        return result.returncode, stdout.read()


def _remove_container(base, name, env):
    status, body = _run([*base, "inspect", "--type", "container", "--format", "{{json .}}", name], env=env, timeout=15)
    if status == 0:
        try:
            record = json.loads(body)
        except (ValueError, TypeError):
            raise RuntimeError("cannot establish fixture container ownership for cleanup") from None
        identity = record.get("Id", "")
        if (record.get("Name") != "/" + name or not re.fullmatch(r"[0-9a-f]{64}", identity)
                or record.get("Config", {}).get("Labels", {}).get("kborg.fixture") != "signature"):
            raise RuntimeError("fixture container cleanup ownership mismatch")
        removed, _ = _run([*base, "rm", "-f", identity], env=env, timeout=30)
        if removed:
            raise RuntimeError("could not remove the owned signature fixture container")
    # A successful daemon listing distinguishes confirmed absence from an
    # unavailable daemon or a failed inspect. Never claim cleanup on uncertainty.
    status, body = _run([*base, "ps", "-a", "--filter", "name=^/" + name + "$", "--format", "{{.Names}}"], env=env, timeout=15)
    if status or body.strip():
        raise RuntimeError("could not confirm signature fixture container removal")


def _unpack(source, destination):
    source = Path(source)
    if source.is_symlink() or not source.is_file() or source.stat().st_size > 8 * 1024 * 1024:
        raise ValueError("fixture chart must be a bounded regular .tgz file")
    with gzip.open(source, "rb") as stream:
        raw = stream.read(32 * 1024 * 1024 + 1)
    if len(raw) > 32 * 1024 * 1024:
        raise ValueError("fixture chart expansion exceeds 32 MiB")
    roots = set()
    with tarfile.open(fileobj=io.BytesIO(raw), mode="r:") as archive:
        count = 0
        for member in archive:
            count += 1
            parts = PurePosixPath(member.name).parts
            if (count > 4096 or not parts or PurePosixPath(member.name).is_absolute()
                    or any(part in (".", "..") for part in parts)
                    or not (member.isfile() or member.isdir())):
                raise ValueError("unsupported fixture chart archive entry")
            roots.add(parts[0])
            target = destination.joinpath(*parts)
            if member.isdir():
                target.mkdir(mode=0o700, parents=True, exist_ok=True)
            else:
                target.parent.mkdir(mode=0o700, parents=True, exist_ok=True)
                data = archive.extractfile(member).read(32 * 1024 * 1024 + 1)
                if len(data) != member.size:
                    raise ValueError("invalid chart entry size")
                with target.open("xb") as stream:
                    target.chmod(0o600)
                    stream.write(data)
    if len(roots) != 1:
        raise ValueError("fixture chart needs one chart root")
    chart = destination / next(iter(roots))
    if not (chart / "Chart.yaml").is_file():
        raise ValueError("fixture chart is missing Chart.yaml")
    return chart


def create(root: Path, charts: dict[str, Path], helm: Path):
    root, helm = Path(root).resolve(), Path(helm).resolve()
    if not charts or not helm.is_file() or not helm.is_absolute():
        raise ValueError("charts and an absolute trusted Helm executable are required")
    root.mkdir(mode=0o700, parents=True, exist_ok=True)
    root.chmod(0o700)
    docker = shutil.which("docker")
    if not docker:
        raise RuntimeError("fixture generation requires Docker; no host GPG installation is performed")
    candidates = [Path.home() / ".docker/run/docker.sock", Path("/var/run/docker.sock")]
    docker_socket = next((path for path in candidates if path.exists() and socket_path(path)), None)
    if docker_socket is None:
        raise RuntimeError("a local Unix Docker socket is required; remote Docker contexts are not used")
    scratch = Path(tempfile.mkdtemp(prefix="signature-private-", dir=root))
    name = "kborg-signature-fixture-" + secrets.token_hex(8)
    private = scratch / "keys"
    private.mkdir(mode=0o700)
    docker_config = scratch / "docker-config"
    docker_config.mkdir(mode=0o700)
    env = {"HOME": str(scratch), "DOCKER_CONFIG": str(docker_config), "PATH": "/usr/bin:/bin:/usr/sbin:/sbin"}
    base = [docker, "--host", "unix://" + str(docker_socket)]
    command = r'''set -eu
export GNUPGHOME=/fixture/gnupg
trap 'gpgconf --kill all >/dev/null 2>&1 || true; chown -R "$FIXTURE_UID:$FIXTURE_GID" /fixture' EXIT
mkdir -m 700 "$GNUPGHOME"
apt-get -qq update >/fixture/apt.log 2>&1
apt-get -qq install --no-install-recommends gnupg >>/fixture/apt.log 2>&1
for label in trusted wrong; do
  if [ "$label" = trusted ]; then identity='KBorg disposable signature fixture <fixture@invalid.example>'; else identity='KBorg wrong signature fixture <wrong@invalid.example>'; fi
  gpg --batch --pinentry-mode loopback --passphrase '' --quick-generate-key "$identity" rsa2048 sign 1d >/fixture/gpg.log 2>&1
  gpg --batch --export "$identity" >"/fixture/$label.gpg"
  if [ "$label" = trusted ]; then gpg --batch --pinentry-mode loopback --passphrase '' --export-secret-keys "$identity" >/fixture/signing.gpg; fi
done
gpgconf --kill all
chown -R "$FIXTURE_UID:$FIXTURE_GID" /fixture
'''
    outputs = []
    result = None
    try:
        status, _ = _run([*base, "run", "--name", name, "--label", "kborg.fixture=signature",
                          "--mount", "type=bind,source=" + str(private) + ",target=/fixture",
                          "--env", "FIXTURE_UID=" + str(os.getuid()), "--env", "FIXTURE_GID=" + str(os.getgid()),
                          IMAGE, "/bin/sh", "-c", command], env=env, timeout=360)
        if status:
            raise RuntimeError("disposable GnuPG fixture generation failed; private diagnostics suppressed")
        helm_home = scratch / "helm"
        helm_home.mkdir(mode=0o700)
        helm_env = {"HOME": str(helm_home), "HELM_CONFIG_HOME": str(helm_home / "config"),
                    "HELM_CACHE_HOME": str(helm_home / "cache"), "HELM_DATA_HOME": str(helm_home / "data"),
                    "HELM_PLUGINS": str(helm_home / "plugins"), "TMPDIR": str(scratch), "LANG": "C",
                    "KUBECONFIG": str(scratch / "no-kubeconfig")}
        signed = {}
        for logical, source in charts.items():
            if not re.fullmatch(r"[a-z][a-z0-9_-]{0,40}", logical):
                raise ValueError("invalid fixture logical name")
            work = scratch / logical
            work.mkdir(mode=0o700)
            unpacked = _unpack(source, work)
            output = work / "output"
            output.mkdir(mode=0o700)
            status, _ = _run([helm, "package", unpacked, "--sign", "--key", IDENTITY,
                              "--keyring", private / "signing.gpg", "--destination", output], env=helm_env)
            packaged = list(output.glob("*.tgz"))
            if status or len(packaged) != 1 or not Path(str(packaged[0]) + ".prov").is_file():
                raise RuntimeError("Helm could not sign the disposable chart fixture")
            chart = packaged[0]
            good, report = _run([helm, "verify", chart, "--keyring", private / "trusted.gpg"], env=helm_env)
            wrong, _ = _run([helm, "verify", chart, "--keyring", private / "wrong.gpg"], env=helm_env)
            if good or not wrong:
                raise RuntimeError("signature fixture positive/negative verification failed")
            fingerprints = re.findall(rb"(?m)^Using Key With Fingerprint: ([A-Fa-f0-9]{40}|[A-Fa-f0-9]{64})$", report)
            if len(fingerprints) != 1:
                raise RuntimeError("Helm fixture verification returned no strict signer fingerprint")
            signed[logical] = chart
        planned = [(path, root / path.name) for chart in signed.values() for path in (chart, Path(str(chart) + ".prov"))]
        planned += [(private / key, root / key) for key in ("trusted.gpg", "wrong.gpg")]
        if len({target for _, target in planned}) != len(planned) or any(target.exists() for _, target in planned):
            raise ValueError("fixture outputs must be new distinct direct-child files")
        for source, target in planned:
            with target.open("xb") as stream:
                target.chmod(0o600)
                stream.write(source.read_bytes())
            outputs.append(target)
        result = {"charts": {key: root / path.name for key, path in signed.items()},
                  "keyring": root / "trusted.gpg", "wrong_keyring": root / "wrong.gpg",
                  "report": {"ephemeral_keys": True, "personal_keyrings_used": False,
                             "trusted_verifier_positive": True, "wrong_key_rejected": True,
                             "signer_fingerprint": fingerprints[0].decode().upper(), "chart_count": len(signed)}}
        manifest = {key: path.name for key, path in result["charts"].items()}
        manifest.update(keyring="trusted.gpg", wrong_keyring="wrong.gpg")
        with (root / "fixtures.json").open("x") as stream:
            json.dump(manifest, stream, indent=2)
        return result
    except BaseException:
        for path in outputs:
            path.unlink(missing_ok=True)
        raise
    finally:
        _remove_container(base, name, env)
        shutil.rmtree(scratch)
        if scratch.exists():
            raise RuntimeError("could not confirm private signing material removal")
        if result is not None:
            result["report"].update(private_signing_material_removed=True, container_removed=True)


def socket_path(path):
    import stat
    return stat.S_ISSOCK(path.stat().st_mode)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--helm", type=Path, required=True)
    parser.add_argument("--chart", action="append", default=[], metavar="NAME=ABS_TGZ")
    args = parser.parse_args()
    charts = {}
    for item in args.chart:
        name, separator, path = item.partition("=")
        if not separator or name in charts:
            parser.error("--chart requires unique NAME=ABS_TGZ")
        charts[name] = Path(path)
    result = create(args.output, charts, args.helm)
    print(json.dumps(result["report"]))


if __name__ == "__main__":
    main()
