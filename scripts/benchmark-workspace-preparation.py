#!/usr/bin/env python3
"""Local file preparation against a warm Git worktree baseline; no agent execution."""
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
import random
import stat
import statistics
from urllib.parse import urlsplit

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('--binary', required=True, type=Path)
parser.add_argument('--psql', required=True, type=Path)
parser.add_argument('--database-url-file', required=True, type=Path,
                    help='private file containing a disposable PostgreSQL connection string')
parser.add_argument('--repository', required=True, type=Path, help='local Git fixture repository')
parser.add_argument('--revision', required=True, help='exact commit to capture')
parser.add_argument('--output', required=True, type=Path)
parser.add_argument('--iterations', type=int, default=5)
parser.add_argument('--seed', type=int, default=42)
parser.add_argument('--git-checkout-workers', type=int, default=0)
parser.add_argument('--max-fixture-bytes', type=int, default=64 * 1024 * 1024)
args = parser.parse_args()
if not 1 <= args.iterations <= 100 or args.git_checkout_workers < 0:
    parser.error('iterations must be 1..100; checkout workers must be nonnegative')
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
        grants = ['--permission', 'read', '--permission', 'repository.write', '--permission', 'workspace.write']
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
        def client(*parts):
            return json.loads(run(BIN, 'client', '--socket', socket,
                                  '--credential-file', credential, *parts))
        profile = client('profile-put', '--key', 'bench-full', '--authority', 'standalone',
                         '--epoch', '1', '--full')['result']['profile_digest']
        node_root = root/'node'
        storage = json.loads(run(BIN, 'node', 'initialize', '--root', node_root,
                                 '--tenant', 'benchmark', '--principal', 'node'))['storage']
        admin('principal-create', '--tenant', 'benchmark', '--principal', 'node', '--kind', 'node')
        admin('node-register', '--tenant', 'benchmark', '--principal', 'node', '--storage', storage)
        node_grants = ['--permission', 'runtime.control']
        admin('access-set', *scope, '--principal', 'node', *node_grants)
        node_credential = root/'node.json'
        admin('token-issue', *scope, '--principal', 'node', *node_grants,
              '--ttl-seconds', '600', '--output', node_credential)
        def node(action, *parts):
            return json.loads(run(BIN, 'node', action, '--config', config,
                                  '--credential-file', node_credential,
                                  '--root', node_root, '--storage', storage, *parts))
        # A private bare clone protects the caller's refs/worktrees and avoids
        # hard-linked objects. Its cost is recorded outside warm preparation.
        mirror = root/'baseline.git'
        git_env = {k:v for k,v in os.environ.items() if not k.startswith('GIT_')}
        git_env.update(GIT_CONFIG_NOSYSTEM='1', GIT_CONFIG_GLOBAL='/dev/null')
        def git(*parts):
            proc = subprocess.run([GIT, '-c', 'core.hooksPath=/dev/null',
                '-c', 'core.autocrlf=false', '-c', 'core.attributesFile=/dev/null',
                '-c', 'checkout.workers='+str(args.git_checkout_workers),
                *map(str, parts)], env=git_env, capture_output=True, timeout=240)
            if proc.returncode:
                raise RuntimeError('Git baseline failed with status '+str(proc.returncode))
            return proc.stdout
        start = time.perf_counter()
        git('clone', '--mirror', '--no-hardlinks', '--template=', repository, mirror)
        mirror_seconds = time.perf_counter()-start
        useful = next((f for f in fixture['files'] if f['mode'] in ('100644','100755')), None)
        if useful is None:
            raise RuntimeError('fixture needs at least one regular file for the useful read')
        def read_useful(path):
            if (path/useful['path']).read_bytes() != bytes.fromhex(useful['hex']):
                raise RuntimeError('first useful read differs from pinned input')
        def verify(path):
            actual = set()
            for parent, dirs, files in os.walk(path, followlinks=False):
                for name in dirs+files:
                    p = Path(parent)/name
                    rel = p.relative_to(path).as_posix()
                    if rel == '.git':
                        continue
                    if not p.is_dir() or p.is_symlink():
                        actual.add(rel)
                # Do not follow a linked directory or inspect Git metadata.
                dirs[:] = [n for n in dirs if n != '.git' and not (Path(parent)/n).is_symlink()]
            if actual != {f['path'] for f in fixture['files']}:
                raise RuntimeError('materialized file set differs')
            for f in fixture['files']:
                p=path/f['path']; mode=p.lstat().st_mode
                if f['mode']=='120000':
                    if not stat.S_ISLNK(mode) or os.fsencode(os.readlink(p)) != bytes.fromhex(f['hex']):
                        raise RuntimeError('symlink differs')
                elif (not stat.S_ISREG(mode) or p.read_bytes()!=bytes.fromhex(f['hex'])
                      or bool(mode & 0o111)!=(f['mode']=='100755')):
                    raise RuntimeError('file bytes or executable bit differ')
        def allocated(path):
            total=path.lstat().st_blocks*512
            for parent, dirs, files in os.walk(path, followlinks=False):
                for name in dirs+files:
                    p=Path(parent)/name
                    if p.relative_to(path).as_posix()!='.git':
                        total+=p.lstat().st_blocks*512
                dirs[:]=[n for n in dirs if n!='.git' and not (Path(parent)/n).is_symlink()]
            return total
        observations=[]
        rng=random.Random(args.seed)
        for wave in range(args.iterations):
            order=['treeharbor','git-worktree']; rng.shuffle(order)
            for system in order:
                preparation_phases = None
                start=time.perf_counter()
                if system=='treeharbor':
                    workspace=str(uuid.uuid4())
                    created_reply=client('workspace-create','--key','create-'+workspace,
                        '--authority','standalone','--epoch','1','--workspace',workspace,
                        '--base',reply['result']['revision'],'--profile-digest',profile)
                    requested = time.perf_counter()
                    prepared=node('prepare-once','--workspace',workspace)
                    materialized = time.perf_counter()
                    if prepared['state']!='files_ready':
                        raise RuntimeError('workspace did not become file-ready')
                    path=Path(prepared['path'])
                    preparation_phases = {
                        'workspace_request_seconds': requested - start,
                        'node_prepare_command_seconds': materialized - requested,
                        'materialization_counters': prepared.get('preparation_statistics'),
                    }
                else:
                    path=root/('git-worktree-'+str(wave))
                    git('-C',mirror,'worktree','add','--detach',path,revision)
                ready=time.perf_counter()-start
                read_useful(path)
                useful_seconds=time.perf_counter()-start
                verify(path)
                disk=allocated(path)
                cleanup_start=time.perf_counter()
                if system=='treeharbor':
                    client('workspace-delete','--key','delete-'+workspace,
                        '--authority','standalone','--epoch','1','--workspace',workspace,
                        '--generation',str(created_reply['result']['generation']))
                    cleaned=node('cleanup-once','--workspace',workspace)
                    if client('workspace-status','--workspace',workspace)['result']['state']!='deleted':
                        raise RuntimeError('workspace cleanup was not acknowledged')
                else:
                    git('-C',mirror,'worktree','remove',path)
                cleanup_seconds=time.perf_counter()-cleanup_start
                observation = {'wave':wave,'order':order.index(system),'system':system,
                    'files_ready_seconds':ready,'first_read_seconds':useful_seconds,
                    'cleanup_seconds':cleanup_seconds,'workspace_allocated_bytes':disk,
                    'verified_bytes_modes_paths':True}
                if preparation_phases is not None:
                    observation['treeharbor_preparation'] = preparation_phases
                observations.append(observation)
        medians={system:{key:statistics.median(o[key] for o in observations if o['system']==system)
                 for key in ['files_ready_seconds','first_read_seconds','cleanup_seconds']}
                 for system in ['treeharbor','git-worktree']}
        report={'version':2,'scope':'local-full-file-preparation',
            'source_revision':revision,'binary_sha256':hashlib.sha256(BIN.read_bytes()).hexdigest(),
            'binary_version':run(BIN,'--version').decode().strip(),
            'platform':platform.platform(),'machine':platform.machine(),'logical_cpus':os.cpu_count(),
            'git_version':git('--version').decode().strip(),
            'postgres_version':sql('SHOW server_version').decode().strip(),
            'tracked_files':len(fixture['files']),'logical_file_bytes':fixture_bytes,
            'iterations':args.iterations,'random_seed':args.seed,
            'git_checkout_workers':args.git_checkout_workers,
            'cache_state':'source objects present; no prepared tree; OS caches uncontrolled',
            'setup':{'snapshot_pack_seconds':pack_seconds,'native_import_seconds':import_seconds,
                     'git_mirror_seconds':mirror_seconds,
                     'treeharbor_objects_allocated_bytes':allocated(store),
                     'git_mirror_allocated_bytes':allocated(mirror)},
            'observations':observations,'sample_medians':medians,
            'limitations':['concurrency 1; full files only; source already loaded',
                'TreeHarbor snapshot vs Git full-history mirror; setup costs not directly equivalent',
                'no runtime, LLM, CI, GitHub, network, cold-cache claim or p95',
                'workspace bytes exclude Git metadata; total database/server CPU/RAM and storage not measured',
                'CLI/node process startup included; no persistent worker daemon',
                'read correctness checked; no untrusted code executed',
                'not a full cost, durability or throughput comparison']}
    finally:
        if server is not None:
            server.terminate()
            server.wait(timeout=15)
        log.close()
        if created:
            sql(f'DROP SCHEMA {schema} CASCADE')
with OUT.open('x') as report_file:
    report_file.write(json.dumps(report,indent=2)+'\n')
print(json.dumps(report['sample_medians'],indent=2))
