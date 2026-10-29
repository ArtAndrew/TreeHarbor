#!/usr/bin/env python3
"""Disposable local native snapshot import via actual released CLI/API protocol."""
import argparse
import platform
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import time
import uuid
import re
from urllib.parse import urlsplit

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('--binary', required=True, type=Path)
parser.add_argument('--psql', required=True, type=Path)
parser.add_argument('--database-url-file', required=True, type=Path,
                    help='private file containing a disposable PostgreSQL connection string')
parser.add_argument('--repository', required=True, type=Path, help='local Git fixture repository')
parser.add_argument('--revision', required=True, help='exact commit to capture')
parser.add_argument('--output', required=True, type=Path)
parser.add_argument('--max-fixture-bytes', type=int, default=64 * 1024 * 1024)
args = parser.parse_args()
if not re.fullmatch(r'(?:[0-9a-fA-F]{40}|[0-9a-fA-F]{64})', args.revision):
    parser.error('revision must be a full Git commit ID')
BIN, PSQL, OUT = args.binary.resolve(), args.psql.resolve(), args.output.resolve()
if args.database_url_file.stat().st_mode & 0o077:
    parser.error('database URL file must not be accessible to group or other users')
URL = args.database_url_file.read_text().strip()
if 'password' in URL.lower() or urlsplit(URL).password is not None:
    parser.error('use PGPASSFILE or local socket authentication, not credentials in the URL')
if OUT.exists():
    parser.error('output already exists; preserve prior measurements')
GIT = shutil.which('git')
if not GIT:
    parser.error('Git is required only to extract the benchmark input snapshot')

def run(*args):
    p = subprocess.run([str(x) for x in args], capture_output=True, timeout=240)
    if p.returncode:
        raise RuntimeError(f'{Path(str(args[0])).name} exited with status {p.returncode}')
    return p.stdout

def sql(text):
    # libpq credentials can come from PGPASSFILE; keep passwords out of the URL.
    result = subprocess.run([str(PSQL), '--dbname', URL, '-At', '-v', 'ON_ERROR_STOP=1', '-c', text],
                            capture_output=True, timeout=240)
    if result.returncode:
        raise RuntimeError('psql failed; check the disposable database configuration')
    return result.stdout

schema = 'th_test_' + uuid.uuid4().hex
server = None
created = False
with tempfile.TemporaryDirectory(prefix='th-import-') as tmp:
    root = Path(tmp)
    config, socket, credential = root/'config.json', root/'service.sock', root/'owner.json'
    log = (root/'service.log').open('wb')
    try:
        sql(f'CREATE SCHEMA {schema}')
        created = True
        store = root/'objects'
        store.mkdir(mode=0o700)
        config.write_text(json.dumps({'database_url': URL, 'schema': schema, 'socket': str(socket),
                                     'storage': {'root': str(store), 'backend': 'benchmark-local'}}))
        config.chmod(0o600)
        scope = ['--tenant', 'benchmark', '--repository', str(uuid.uuid4())]
        def admin(*args):
            return run(BIN, 'admin', '--config', config, *args)
        admin('migrate')
        admin('repository-create', *scope, '--authority', 'standalone')
        admin('principal-create', '--tenant', 'benchmark', '--principal', 'owner', '--kind', 'human')
        grants = ['--permission', 'read', '--permission', 'repository.write']
        admin('access-set', *scope, '--principal', 'owner', *grants)
        admin('token-issue', *scope, '--principal', 'owner', *grants, '--ttl-seconds', '600', '--output', credential)
        server = subprocess.Popen([str(BIN), 'serve', '--config', str(config)], stdout=log, stderr=log)
        deadline = time.monotonic() + 15
        while not socket.exists():
            assert server.poll() is None and time.monotonic() < deadline
            time.sleep(.05)
        repository = args.repository.resolve()
        revision = run(GIT, '-C', repository, 'rev-parse', '--verify', args.revision + '^{commit}').decode().strip()
        entries = run(GIT, '-C', repository, 'ls-tree', '-r', '-l', '-z', revision).split(b'\0')
        fixture_bytes = 0
        fixture = {'revision': revision, 'files': []}
        for entry in filter(None, entries):
            meta, path = entry.split(b'\t', 1)
            mode, kind, oid, size = meta.split()
            if kind != b'blob' or mode not in (b'100644', b'100755', b'120000'):
                raise RuntimeError('fixture contains an unsupported entry such as a submodule')
            fixture_bytes += int(size)
            if fixture_bytes > args.max_fixture_bytes:
                raise RuntimeError('fixture exceeds the configured byte limit')
            fixture['files'].append({'path': path.decode('utf-8'), 'mode': mode.decode(),
                                    'hex': run(GIT, '-C', repository, 'cat-file', 'blob', oid.decode()).hex()})
        if not fixture['files']:
            raise RuntimeError('fixture must contain files')
        source = root/'source'
        source.mkdir(mode=0o700)
        paths = []
        for f in fixture['files']:
            path = source/f['path']
            assert path.is_relative_to(source) and '..' not in Path(f['path']).parts
            path.parent.mkdir(parents=True, exist_ok=True)
            data = bytes.fromhex(f['hex'])
            if f['mode'] == '120000':
                os.symlink(os.fsdecode(data), path)
            else:
                path.write_bytes(data)
                path.chmod(0o700 if f['mode']=='100755' else 0o600)
            paths.extend(['--path', f['path']])
        paths = [part for name in sorted({f['path'].split('/')[0] for f in fixture['files']}) for part in ('--path', name)]
        if len(paths) // 2 > 1024:
            raise RuntimeError('fixture has more than 1024 top-level selection paths')
        start = time.perf_counter()
        pack_reply = json.loads(run(BIN, 'source-pack', '--directory', source, '--output', root/'pack',
             '--author-name', 'Benchmark', '--author-email', 'benchmark@example.invalid', *paths))
        pack_seconds = time.perf_counter()-start
        command = [BIN, 'client', '--socket', socket, '--credential-file', credential,
                   'source-import', '--pack', root/'pack', '--state', root/'import.json',
                   '--authority', 'standalone', '--epoch', '1']
        start = time.perf_counter()
        reply = json.loads(run(*command))
        import_seconds = time.perf_counter()-start
        assert reply['result']['state']=='completed'
        start = time.perf_counter()
        replay = json.loads(run(*command))
        replay_seconds = time.perf_counter()-start
        assert replay['result']==reply['result']
        report = {'version':1, 'source_revision': fixture['revision'],
                  'binary_sha256':hashlib.sha256(BIN.read_bytes()).hexdigest(),
                  'platform':platform.platform(),
                  'git_version':run(GIT, '--version').decode().strip(),
                  'tracked_files':len(fixture['files']), 'pack_seconds':pack_seconds,
                  'import_seconds':import_seconds, 'completed_replay_seconds':replay_seconds,
                  'exact_completed_receipt_replayed':True,
                  'pack':{k:v for k,v in pack_reply.items() if k not in ('pack','pack_path_hex')},
                  'limitations':['one local optimized CLI/service observation',
                                 'snapshot import, not full Git history', 'no cloud/network/LLM',
                                 'temporary isolated schema/credentials removed after run']}
    finally:
        if server is not None:
            server.terminate()
            server.wait(timeout=15)
        log.close()
        if created:
            sql(f'DROP SCHEMA {schema} CASCADE')
with OUT.open('x') as report_file:
    report_file.write(json.dumps(report,indent=2)+'\n')
print(json.dumps({k:report[k] for k in ['pack_seconds','import_seconds','completed_replay_seconds']}))
