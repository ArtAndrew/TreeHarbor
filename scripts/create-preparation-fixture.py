#!/usr/bin/env python3
"""Create a small deterministic Git fixture in a NEW directory; no user code runs."""
import argparse
import os
from pathlib import Path
import subprocess

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('directory', type=Path)
args = parser.parse_args()
root = args.directory.resolve()
root.mkdir(mode=0o700)  # Refuse existing output, including a symlink.
for module in range(8):
    directory = root / f'module-{module}'
    directory.mkdir()
    for index in range(16):
        data = f'module={module} file={index}\n'.encode() + b'x' * 4096
        (directory / f'file-{index:02}.txt').write_bytes(data[:4096])
(root / 'README.txt').write_text('Synthetic TreeHarbor file preparation fixture. No build or agent execution.\n')
(root / 'run.sh').write_text('#!/bin/sh\nexit 0\n')
(root / 'run.sh').chmod(0o755)
os.symlink('README.txt', root / 'readme-link')
env = {k: v for k, v in os.environ.items() if not k.startswith('GIT_')}
env.update(GIT_CONFIG_NOSYSTEM='1', GIT_CONFIG_GLOBAL='/dev/null',
           GIT_AUTHOR_NAME='TreeHarbor benchmark', GIT_COMMITTER_NAME='TreeHarbor benchmark',
           GIT_AUTHOR_EMAIL='benchmark@example.invalid', GIT_COMMITTER_EMAIL='benchmark@example.invalid',
           GIT_AUTHOR_DATE='2026-11-02T00:00:00+00:00', GIT_COMMITTER_DATE='2026-11-02T00:00:00+00:00')
def git(*parts):
    return subprocess.check_output(['git', '-c', 'core.hooksPath=/dev/null',
        '-c', 'core.autocrlf=false', '-c', 'core.attributesFile=/dev/null',
        '-C', str(root), *parts], env=env, text=True).strip()
git('init', '--object-format=sha1', '--initial-branch=main', '--template=')
git('add', '.')
git('commit', '-m', 'Synthetic pinned file fixture')
print(git('rev-parse', 'HEAD'))
