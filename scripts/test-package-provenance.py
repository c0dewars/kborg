#!/usr/bin/env python3
"""Negative provenance checks with isolated Git repositories and explicit tool stubs.

These fixtures prove rejection paths, not a native compiler or release artifact.
A real clean-checkout package must be built and smoke-tested separately.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile

ROOT = Path(__file__).resolve().parent.parent
TARGET = 'aarch64-apple-darwin'


def run(argv, cwd, env=None):
    return subprocess.run(argv, cwd=cwd, env=env, text=True, capture_output=True, timeout=30)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--pwsh', type=Path, required=True)
    parser.add_argument('--output', type=Path, required=True)
    args = parser.parse_args()
    pwsh = args.pwsh.resolve(strict=True)
    checks = []
    with tempfile.TemporaryDirectory(prefix='kborg-package-provenance-') as temporary:
        scratch = Path(temporary).resolve()
        for case in ['wrong-commit', 'dirty-tracked', 'untracked-file', 'ignored-packaged-file',
                     'linked-packaged-file', 'wrong-native-host', 'fresh-build-version-mismatch']:
            repo = scratch / case
            repo.mkdir()
            inputs = ['README.md', 'LICENSE', 'THIRD_PARTY_NOTICES.md', 'SECURITY.md',
                      'docs/RELEASE_NOTES.md', 'assets/kborg-logo.png', 'assets/LOGO.md',
                      'assets/upstream/NOTICE', 'assets/dependency-licenses/NOTICE']
            for name in inputs:
                path = repo / name
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_text('fixture only\n')
            (repo / '.gitignore').write_text('/dist/\n/target/\n*.tmp\n')
            scripts = repo / 'scripts'
            scripts.mkdir()
            shutil.copyfile(ROOT / 'scripts/Package-Release.ps1', scripts / 'Package-Release.ps1')
            for command in [['git', 'init', '-q'], ['git', 'add', '.'],
                            ['git', '-c', 'user.name=Packaging Fixture', '-c', 'user.email=fixture@example.invalid',
                             'commit', '-qm', 'Synthetic package fixture']]:
                result = run(command, repo)
                assert result.returncode == 0, 'fixture Git setup failed'
            commit = run(['git', 'rev-parse', 'HEAD'], repo).stdout.strip()
            tool_directory = scratch / (case + '-tools')
            tool_directory.mkdir()
            log = scratch / (case + '-cargo.jsonl')
            compiler_host = 'x86_64-unknown-linux-gnu' if case == 'wrong-native-host' else TARGET
            (tool_directory / 'rustc').write_text('#!/bin/sh\nprintf "rustc fixture\\nhost: ' + compiler_host + '\\n"\n')
            cargo_stub = r'''#!/usr/bin/env python3
import json,os,pathlib,sys
with open(os.environ['KBORG_PACKAGING_TEST_LOG'],'a') as log: log.write(json.dumps(sys.argv[1:])+'\n')
if sys.argv[1]=='metadata':
 print(json.dumps({'packages':[{'id':'fixture-kborg','name':'kborg','version':'0.3.0','manifest_path':str(pathlib.Path.cwd()/'Cargo.toml'),'license':'Apache-2.0'}],'resolve':{'nodes':[]}}))
elif sys.argv[1]=='build':
 target=sys.argv[sys.argv.index('--target')+1]; directory=pathlib.Path(sys.argv[sys.argv.index('--target-dir')+1]); binary=directory/target/'release'/'kborg';binary.parent.mkdir(parents=True);binary.write_text('#!/bin/sh\nprintf "KBorg 9.9.9\\n"\n');binary.chmod(0o700)
else: sys.exit(91)
'''
            (tool_directory / 'cargo').write_text(cargo_stub)
            for name in ['rustc', 'cargo']:
                (tool_directory / name).chmod(0o700)
            env = dict(os.environ, PATH=str(tool_directory) + os.pathsep + os.environ['PATH'],
                       KBORG_PACKAGING_TEST_LOG=str(log), POWERSHELL_TELEMETRY_OPTOUT='1')
            for name in ['RUSTFLAGS', 'CARGO_ENCODED_RUSTFLAGS', 'RUSTC', 'RUSTC_WRAPPER',
                         'RUSTC_WORKSPACE_WRAPPER', 'CARGO_BUILD_RUSTC', 'CARGO_BUILD_RUSTC_WRAPPER',
                         'CARGO_BUILD_RUSTC_WORKSPACE_WRAPPER', 'CARGO_BUILD_RUSTFLAGS']:
                env.pop(name, None)
            expected = ''
            if case == 'wrong-commit':
                commit = '0' * 40
                expected = 'SourceCommit must equal'
            elif case == 'dirty-tracked':
                (repo / 'README.md').write_text('changed source\n')
                expected = 'clean checkout'
            elif case == 'untracked-file':
                (repo / 'docs/untracked.txt').write_text('untracked input\n')
                expected = 'clean checkout'
            elif case == 'ignored-packaged-file':
                (repo / 'docs/ignored.tmp').write_text('ignored but recursively packaged\n')
                expected = 'Every package input must be a tracked plain file'
            elif case == 'linked-packaged-file':
                linked = repo / 'docs/linked.txt'
                linked.symlink_to(repo / 'README.md')
                assert run(['git', 'add', '.'], repo).returncode == 0
                assert run(['git', '-c', 'user.name=Packaging Fixture', '-c', 'user.email=fixture@example.invalid',
                            'commit', '-qm', 'Add synthetic linked input'], repo).returncode == 0
                commit = run(['git', 'rev-parse', 'HEAD'], repo).stdout.strip()
                expected = 'Linked package inputs are forbidden'
            elif case == 'wrong-native-host':
                expected = 'native Rust host target'
            else:
                stale = repo / 'target' / TARGET / 'release/kborg'
                stale.parent.mkdir(parents=True)
                stale.write_text('#!/bin/sh\nexit 92\n')
                stale.chmod(0o700)
                expected = 'Binary version must exactly match'
            result = run([str(pwsh), '-NoLogo', '-NoProfile', '-File', str(scripts / 'Package-Release.ps1'),
                          '-Target', TARGET, '-SourceCommit', commit], repo, env)
            output = result.stdout + result.stderr
            assert result.returncode != 0 and expected in output, case + ' did not reject correctly: ' + output
            calls = [] if not log.exists() else [json.loads(line) for line in log.read_text().splitlines()]
            if case != 'fresh-build-version-mismatch':
                assert not calls, case + ' reached Cargo before provenance rejection'
            else:
                assert len(calls) == 2 and calls[0][0] == 'metadata' and calls[1][:4] == ['build', '--release', '--locked', '--target']
                built = Path(calls[1][calls[1].index('--target-dir') + 1])
                assert built.parent == repo / 'dist/build' and not built.exists(), 'fresh build directory not isolated/cleaned'
            assert not list((repo / 'dist/packages').glob('*.tar.gz')), 'rejected package produced archive'
            checks.append({'case': case, 'passed': True})
        archive = scratch / 'test.archive'
        archive.write_bytes(b'synthetic archive integrity fixture\n')
        digest = hashlib.sha256(archive.read_bytes()).hexdigest()
        for supplied, passed in [(digest, True), ('0' * 64, False)]:
            result = run([str(pwsh), '-NoLogo', '-NoProfile', '-File', str(ROOT / 'scripts/Verify-Package.ps1'),
                          '-Archive', str(archive), '-ExpectedSha256', supplied], ROOT)
            assert (result.returncode == 0) == passed
            if passed:
                assert 'not publisher authentication' in result.stdout
            checks.append({'case': 'trusted-digest-match' if passed else 'digest-mismatch', 'passed': True})
    report = {'schema': 1, 'passed': True, 'checks': checks,
              'packager_sha256': hashlib.sha256((ROOT / 'scripts/Package-Release.ps1').read_bytes()).hexdigest(),
              'verifier_sha256': hashlib.sha256((ROOT / 'scripts/Verify-Package.ps1').read_bytes()).hexdigest(),
              'scope': 'Negative source/native-build/version preflight with synthetic Git/tool fixtures and real PowerShell; digest verification with synthetic bytes. Does not claim a native release build or publisher authenticity.'}
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(report, indent=2) + '\n')
    print(json.dumps({'passed': True, 'checks': len(checks)}))


if __name__ == '__main__':
    main()
