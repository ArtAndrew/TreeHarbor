#!/usr/bin/env python3
"""Local full-file preparation against Git worktree and clone baselines."""
import argparse
from concurrent.futures import ThreadPoolExecutor
from contextlib import ExitStack
from math import ceil
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
parser.add_argument('--psql', type=Path)
parser.add_argument('--database-url-file', type=Path,
                    help='private file containing a disposable PostgreSQL connection string')
parser.add_argument('--connected-fixture', type=Path, help=argparse.SUPPRESS)
parser.add_argument('--repository', required=True, type=Path, help='local Git fixture repository')
parser.add_argument('--revision', required=True, help='exact commit to capture')
parser.add_argument('--output', required=True, type=Path)
parser.add_argument('--iterations', type=int, default=5)
parser.add_argument('--concurrency', type=int, default=1)
parser.add_argument('--seed', type=int, default=42)
parser.add_argument('--git-checkout-workers', type=int, default=0)
parser.add_argument('--max-fixture-bytes', type=int, default=64 * 1024 * 1024)
parser.add_argument('--workspace-volume', type=Path, action='append', default=[],
                    help='one dedicated disposable ext4 mount per concurrent TreeHarbor task')
parser.add_argument('--volume-limit-bytes', type=int, default=128 * 1024 * 1024)
parser.add_argument('--cow-driver', type=Path, help=argparse.SUPPRESS)
parser.add_argument('--cow-shared-node', action='store_true', help=argparse.SUPPRESS)
parser.add_argument('--cow-source-volume', type=Path, action='append', default=[], help=argparse.SUPPRESS)
args = parser.parse_args()
if not 1 <= args.iterations <= 100 or args.git_checkout_workers < 0:
    parser.error('iterations must be 1..100; checkout workers must be nonnegative')
if not 1 <= args.concurrency <= 16:
    parser.error('concurrency must be 1..16 for this local probe')
volumes = [path.resolve() for path in args.workspace_volume]
if volumes and (platform.system() != 'Linux' or len(volumes) != args.concurrency
                or len(set(volumes)) != len(volumes) or args.volume_limit_bytes <= 0
                or not all(os.path.ismount(mount) for mount in volumes)):
    parser.error('provide one distinct disposable Linux mount per task and a positive volume limit')
if args.cow_driver and (not volumes or not args.cow_driver.is_absolute()):
    parser.error('the local overlay experiment requires guarded volumes and an absolute driver path')
if args.cow_shared_node and not args.cow_driver:
    parser.error('shared preparation requires the local overlay experiment')
source_volumes = [path.resolve() for path in args.cow_source_volume]
if source_volumes and (not args.cow_shared_node
                       or len(source_volumes) != (2 if args.connected_fixture else 1)
                       or len(set(source_volumes)) != len(source_volumes)
                       or set(source_volumes) & set(volumes)
                       or not all(path.is_mount() for path in source_volumes)):
    parser.error('provide a distinct dedicated source mount per format on the shared node')
if not re.fullmatch(r'(?:[0-9a-fA-F]{40}|[0-9a-fA-F]{64})', args.revision):
    parser.error('revision must be a full Git commit ID')
BIN, OUT = args.binary.resolve(), args.output.resolve()
connected = None
if args.connected_fixture:
    if args.psql or args.database_url_file or args.connected_fixture.stat().st_mode & 0o077:
        parser.error('use only the private Connected context supplied by the transport fixture')
    connected = json.loads(args.connected_fixture.read_text())
else:
    if not args.psql or not args.database_url_file:
        parser.error('provide --psql and --database-url-file for the standalone benchmark')
    PSQL = args.psql.resolve()
    if args.database_url_file.stat().st_mode & 0o077:
        parser.error('database URL file must not be accessible to group or other users')
    URL = args.database_url_file.read_text().strip()
    if 'password' in URL.lower() or urlsplit(URL).password is not None:
        parser.error('use PGPASSFILE or local socket authentication, not credentials in the URL')
if OUT.exists():
    parser.error('output already exists; preserve prior measurements')
GIT = shutil.which('git')
if not GIT:
    parser.error('Git is required for the benchmark input and baselines')

authority = 'connected' if connected else 'standalone'
SYSTEMS = (('treeharbor', 'treeharbor-git', 'git-worktree') if connected
           else ('treeharbor', 'git-worktree', 'git-shared-clone', 'git-full-clone'))
TIMINGS = ('files_ready_seconds', 'first_read_seconds', 'cleanup_seconds')

def run(*args):
    p = subprocess.run([str(x) for x in args], capture_output=True, timeout=240)
    if p.returncode:
        if os.environ.get('TH_BENCH_DIAGNOSTICS') == '1' and len(args) > 2 and args[1] == 'node':
            print(p.stderr.decode(errors='replace')[-1000:], flush=True)
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
with tempfile.TemporaryDirectory(prefix='th-import-') as tmp, ExitStack() as placements:
    root = Path(tmp)
    config, socket, credential = root/'config.json', root/'service.sock', root/'owner.json'
    log = None
    try:
        def admin(*args):
            return run(BIN, 'admin', '--config', config, *args)
        if connected:
            config, socket, credential, store = (Path(connected[key]) for key in
                                                   ('config', 'socket', 'credential', 'store'))
            tenant, repo_id = connected['tenant'], connected['repository_id']
        else:
            tenant, repo_id = 'benchmark', str(uuid.uuid4())
            sql(f'CREATE SCHEMA {schema}')
            created = True
            store = root/'objects'
            store.mkdir(mode=0o700)
            config.write_text(json.dumps({'database_url': URL, 'schema': schema, 'socket': str(socket),
                                         'storage': {'root': str(store), 'backend': 'benchmark-local'}}))
            config.chmod(0o600)
            scope = ['--tenant', tenant, '--repository', repo_id]
            admin('migrate')
            admin('repository-create', *scope, '--authority', authority)
            admin('principal-create', '--tenant', tenant, '--principal', 'owner', '--kind', 'human')
            grants = ['--permission', 'read', '--permission', 'repository.write', '--permission', 'workspace.write']
            admin('access-set', *scope, '--principal', 'owner', *grants)
            admin('token-issue', *scope, '--principal', 'owner', *grants, '--ttl-seconds', '600', '--output', credential)
            log = (root/'service.log').open('wb')
            server = subprocess.Popen([str(BIN), 'serve', '--config', str(config)], stdout=log, stderr=log)
            deadline = time.monotonic() + 15
            while not socket.exists():
                assert server.poll() is None and time.monotonic() < deadline
                time.sleep(.05)
        scope = ['--tenant', tenant, '--repository', repo_id]
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
        setup = {}
        if connected:
            if connected['git_revision'].rsplit(':', 1)[-1] != revision:
                raise RuntimeError('the imported Git commit differs from the benchmark fixture')
            native_revision = connected['native_revision']
        else:
            source = root/'source'
            source.mkdir(mode=0o700)
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
            names = sorted({f['path'].split('/')[0] for f in fixture['files']})
            if len(names) > 1024:
                raise RuntimeError('fixture has more than 1024 top-level selection paths')
            paths = [part for name in names for part in ('--path', name)]
            start = time.perf_counter()
            json.loads(run(BIN, 'source-pack', '--directory', source, '--output', root/'pack',
                 '--author-name', 'Benchmark', '--author-email', 'benchmark@example.invalid', *paths))
            setup['snapshot_pack_seconds'] = time.perf_counter()-start
            command = [BIN, 'client', '--socket', socket, '--credential-file', credential,
                       'source-import', '--pack', root/'pack', '--state', root/'import.json',
                       '--authority', authority, '--epoch', '1']
            start = time.perf_counter()
            reply = json.loads(run(*command))
            setup['native_import_seconds'] = time.perf_counter()-start
            assert reply['result']['state']=='completed'
            replay = json.loads(run(*command))
            assert replay['result']==reply['result']
            native_revision = reply['result']['revision']
        def client(*parts):
            return json.loads(run(BIN, 'client', '--socket', socket,
                                  '--credential-file', credential, *parts))
        profile = client('profile-put', '--key', 'bench-full', '--authority', authority,
                         '--epoch', '1', '--full')['result']['profile_digest']
        node_grants = ['--permission', 'runtime.control']
        node_systems = ('treeharbor', 'treeharbor-git') if connected else ('treeharbor',)
        nodes = {}
        for system in node_systems:
            for lane in range(args.concurrency):
                if args.cow_shared_node and lane:
                    nodes[system, lane] = nodes[system, 0]
                    continue
                principal = f'{system}-node-{lane}'
                node_root = root/principal
                storage = json.loads(run(BIN, 'node', 'initialize', '--root', node_root,
                                         '--tenant', tenant, '--principal', principal))['storage']
                admin('principal-create', '--tenant', tenant, '--principal', principal, '--kind', 'node')
                admin('node-register', '--tenant', tenant, '--principal', principal, '--storage', storage)
                admin('access-set', *scope, '--principal', principal, *node_grants)
                node_credential = root/(principal + '.json')
                admin('token-issue', *scope, '--principal', principal, *node_grants,
                      '--ttl-seconds', '600', '--output', node_credential)
                nodes[system, lane] = (node_root, storage, node_credential)

        def node(system, lane, action, *parts):
            node_root, storage, node_credential = nodes[system, lane]
            placement = ('--cow-driver', args.cow_driver) if args.cow_driver and action in (
                'prepare-once', 'prepare-batch', 'cleanup-once') else ()
            return json.loads(run(BIN, 'node', action, '--config', config,
                                  '--credential-file', node_credential,
                                  '--root', node_root, '--storage', storage, *parts, *placement))
        for system, mount in zip(node_systems, source_volumes):
            node(system, 0, 'cow-source-register', '--mount', mount,
                 '--limit-bytes', args.volume_limit_bytes)
            # Registration is an immutable idempotent mapping, not a new volume.
            node(system, 0, 'cow-source-register', '--mount', mount,
                 '--limit-bytes', args.volume_limit_bytes)
        slots, git_roots = {}, []
        for lane, mount in enumerate(volumes):
            git_roots.append(Path(placements.enter_context(tempfile.TemporaryDirectory(prefix='th-bench-git-', dir=mount))))
            for system in node_systems:
                placement = placements.enter_context(tempfile.TemporaryDirectory(prefix='th-bench-', dir=mount))
                slot = str(uuid.uuid4())
                node(system, lane, 'volume-register', '--slot', slot, '--mount', mount,
                     '--placement', placement, '--limit-bytes', args.volume_limit_bytes)
                slots[system, lane] = slot
        if args.cow_driver:
            def release_fixture_mounts():
                # No task is executed by this fixture. Preserve the initial
                # exception without letting temporary-directory removal walk
                # through a mounted view from a failed preparation.
                for path in root.glob('.treeharbor-cow-*/views/*'):
                    if path.is_mount():
                        run('/usr/bin/sudo', '-n', '/usr/bin/umount', '--', path)
            placements.callback(release_fixture_mounts)
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
                if os.environ.get('TH_BENCH_DIAGNOSTICS') == '1':
                    print(proc.stderr.decode(errors='replace')[-1000:], flush=True)
                raise RuntimeError('Git baseline failed with status '+str(proc.returncode))
            return proc.stdout
        start = time.perf_counter()
        git('clone', '--mirror', '--no-hardlinks', '--template=', repository, mirror)
        mirror_seconds = time.perf_counter()-start
        expected_history = git('-C', mirror, 'rev-list', revision) if connected else None
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
        def allocated(path, exclude_git=True):
            total=path.lstat().st_blocks*512
            for parent, dirs, files in os.walk(path, followlinks=False):
                for name in dirs+files:
                    p=Path(parent)/name
                    if not exclude_git or p.relative_to(path).as_posix()!='.git':
                        total+=p.lstat().st_blocks*512
                dirs[:]=[n for n in dirs if (not exclude_git or n!='.git') and not (Path(parent)/n).is_symlink()]
            return total
        def request_workspace(system):
            workspace = str(uuid.uuid4())
            format_args = ['--git-compatible'] if system == 'treeharbor-git' else []
            created = client('workspace-create', '--key', 'create-'+workspace,
                '--authority', authority, '--epoch', '1', '--workspace', workspace,
                '--base', native_revision, '--profile-digest', profile, *format_args)
            return workspace, created['result']['generation']

        def observed_path(observation, prepared, workspace, start, requested, materialized):
            if prepared['state'] != 'files_ready' or prepared['workspace'] != workspace:
                if os.environ.get('TH_BENCH_DIAGNOSTICS') == '1':
                    print('Unexpected preparation state:', prepared.get('state'), prepared.get('error'), flush=True)
                raise RuntimeError('the selected workspace did not become file-ready')
            if args.cow_driver:
                if prepared.get('placement') != 'copy-on-write':
                    raise RuntimeError('the selected workspace did not use the mounted view')
                attempt = Path(prepared['manifest']).parent.parent
                observation['overlay_writable_allocated_bytes'] = sum(
                    allocated(attempt / name, exclude_git=False)
                    for name in ('overlay-upper', 'overlay-work')
                ) if os.environ.get('TH_BENCH_MEASURE_UPPER') == '1' else None
            observation['treeharbor_preparation'] = {
                'workspace_request_seconds': requested - start,
                'node_prepare_command_seconds': materialized - requested,
                'materialization_counters': prepared.get('preparation_statistics'),
            }
            return Path(prepared['path'])

        def prepare(system, wave, order, lane):
            observation = {'wave': wave, 'order': order, 'lane': lane, 'system': system}
            workspace = generation = None
            start = time.perf_counter()
            if system in node_systems:
                workspace, generation = request_workspace(system)
                requested = time.perf_counter()
                slot_args = ['--slot', slots[system, lane]] if slots else []
                git_args = ['--git', Path(GIT).absolute()] if system == 'treeharbor-git' else []
                prepared = node(system, lane, 'prepare-once', '--workspace', workspace, *slot_args, *git_args)
                materialized = time.perf_counter()
                path = observed_path(observation, prepared, workspace, start, requested, materialized)
            else:
                destination = git_roots[lane] if git_roots else root
                path = destination/f'{system}-{wave}-{lane}'
                if system == 'git-worktree':
                    git('-C', mirror, 'worktree', 'add', '--detach', path, revision)
                else:
                    method = '--shared' if system == 'git-shared-clone' else '--no-local'
                    git('clone', method, '--no-checkout', '--template=', mirror, path)
                    git('-C', path, 'checkout', '--detach', revision)
            observation['files_ready_seconds'] = time.perf_counter() - start
            read_useful(path)
            observation['first_read_seconds'] = time.perf_counter() - start
            return path, workspace, generation, observation

        def prepare_shared(system, wave, order, workers):
            start = time.perf_counter()
            requests = list(workers.map(request_workspace, [system] * args.concurrency))
            requested = time.perf_counter()
            selection = root / f'prepare-{system}-{wave}.json'
            targets = [{'workspace': workspace, 'slot': slots[system, lane]}
                       for lane, (workspace, _) in enumerate(requests)]
            fd = os.open(selection, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
            with os.fdopen(fd, 'w') as output:
                json.dump(targets, output)
            git_args = ['--git', Path(GIT).absolute()] if system == 'treeharbor-git' else []
            results = node(system, 0, 'prepare-batch', '--selection', selection,
                           '--parallelism', str(args.concurrency), *git_args)
            materialized = time.perf_counter()
            by_workspace = {row['workspace']: row for row in results}
            if len(results) != len(by_workspace) or set(by_workspace) != {r['workspace'] for r in targets}:
                raise RuntimeError('shared preparation response does not match its exact requests')
            if len({row.get('path') for row in results}) != args.concurrency:
                raise RuntimeError('shared preparation did not publish distinct placements')
            if sum(row.get('preparation_statistics') is not None for row in results) != int(wave == 0):
                raise RuntimeError('shared preparation did not build exactly one source image')
            prepared = []
            for lane, (workspace, generation) in enumerate(requests):
                observation = {'wave': wave, 'order': order, 'lane': lane, 'system': system}
                path = observed_path(observation, by_workspace[workspace], workspace, start, requested, materialized)
                observation['files_ready_seconds'] = materialized - start
                read_useful(path)
                observation['first_read_seconds'] = time.perf_counter() - start
                prepared.append((path, workspace, generation, observation))
            return prepared

        def cleanup(case):
            path, workspace, generation, observation = case
            start = time.perf_counter()
            system = observation['system']
            if system in node_systems:
                client('workspace-delete', '--key', 'delete-'+workspace,
                    '--authority', authority, '--epoch', '1', '--workspace', workspace,
                    '--generation', str(generation))
                node(system, observation['lane'], 'cleanup-once', '--workspace', workspace)
                if client('workspace-status', '--workspace', workspace)['result']['state'] != 'deleted':
                    raise RuntimeError('workspace cleanup was not acknowledged')
            elif system == 'git-worktree':
                git('-C', mirror, 'worktree', 'remove', path)
            else:
                shutil.rmtree(path)
            if path.exists():
                raise RuntimeError('workspace remains after cleanup')
            observation['cleanup_seconds'] = time.perf_counter() - start
            return observation

        observations = []
        batches = []
        rng = random.Random(args.seed)
        with ThreadPoolExecutor(max_workers=args.concurrency) as workers:
            for wave in range(args.iterations):
                order = list(SYSTEMS)
                rng.shuffle(order)
                for index, system in enumerate(order):
                    start = time.perf_counter()
                    shared = args.cow_shared_node and system in node_systems
                    if shared:
                        prepared_cases = prepare_shared(system, wave, index, workers)
                    else:
                        prepared_cases = list(workers.map(
                            lambda lane: prepare(system, wave, index, lane), range(args.concurrency)))
                    batches.append({'wave': wave, 'order': index, 'system': system,
                                    'all_first_reads_seconds': time.perf_counter() - start})
                    for path, _, _, observation in prepared_cases:
                        verify(path)
                        observation['workspace_allocated_bytes'] = allocated(path)
                        observation['git_metadata_allocated_bytes'] = 0
                        if system != 'treeharbor':
                            git_dir = Path(os.fsdecode(git('-C', path, 'rev-parse', '--absolute-git-dir').rstrip(b'\n')))
                            observation['git_metadata_allocated_bytes'] = allocated(git_dir)
                            if system == 'git-worktree':
                                observation['git_metadata_allocated_bytes'] += allocated(path/'.git')
                            if connected:
                                if (git('-C', path, 'rev-parse', 'HEAD').decode().strip() != revision
                                        or git('-C', path, 'rev-list', 'HEAD') != expected_history):
                                    raise RuntimeError('Git workspace history differs from the fixture')
                                if system == 'treeharbor-git':
                                    if (git_dir != (path/'.git').resolve() or (path/'.git').is_symlink()
                                            or (git_dir/'objects/info/alternates').exists()
                                            or git('-C', path, 'remote')
                                            or git('-C', path, 'status', '--porcelain', '--untracked-files=all')):
                                        raise RuntimeError('Git-compatible workspace is not private and clean')
                                    git('-C', path, 'fsck', '--strict', '--no-dangling')
                                observation['verified_git_head_and_history'] = True
                        elif connected and (path/'.git').exists():
                            raise RuntimeError('file-only workspace contains Git metadata')
                        observation['verified_bytes_modes_paths'] = True
                    observations.extend(map(cleanup, prepared_cases) if shared
                                        else workers.map(cleanup, prepared_cases))

        def summarize(system, statistic):
            return {key: statistic([o[key] for o in observations if o['system'] == system])
                    for key in TIMINGS}

        def sample_p95(values):
            return sorted(values)[ceil(0.95 * len(values)) - 1] if len(values) >= 20 else None

        medians = {system: summarize(system, statistics.median) for system in SYSTEMS}
        tails = {system: summarize(system, sample_p95) for system in SYSTEMS}
        report={'version':3,'scope':'local-connected-preparation' if connected else 'local-full-file-preparation',
            'source_revision':revision,'binary_sha256':hashlib.sha256(BIN.read_bytes()).hexdigest(),
            'binary_version':run(BIN,'--version').decode().strip(),
            'platform':platform.platform(),'machine':platform.machine(),'logical_cpus':os.cpu_count(),
            'git_version':git('--version').decode().strip(),
            'postgres_version':connected['postgres_version'] if connected else sql('SHOW server_version').decode().strip(),
            'tracked_files':len(fixture['files']),'logical_file_bytes':fixture_bytes,
            'iterations':args.iterations,'concurrency':args.concurrency,'random_seed':args.seed,
            'git_checkout_workers':args.git_checkout_workers,
            'treeharbor_placement':'overlay-on-guarded-ext4' if args.cow_driver else ('guarded-ext4' if slots else 'node-directory'),
            'treeharbor_nodes_per_system':1 if args.cow_shared_node else args.concurrency,
            'cache_state':('source objects present; initially empty node per system; OS caches uncontrolled'
                if args.cow_shared_node else
                'source objects present; separate initially empty nodes by system/lane; OS caches uncontrolled'),
            'setup':{**setup,'git_mirror_seconds':mirror_seconds,
                     'treeharbor_objects_allocated_bytes':allocated(store),
                     'git_mirror_allocated_bytes':allocated(mirror)},
            'observations':observations,'batches':batches,'sample_medians':medians,'sample_p95':tails,
            'limitations':['full files only; source already loaded on the same host',
                ('Connected import completes before the measured phase' if connected
                 else 'TreeHarbor snapshot vs Git full-history mirror; setup costs not directly equivalent'),
                'no runtime, LLM, CI, GitHub, measured transport bytes or cold-cache claim',
                'p95 is a nearest-rank sample statistic and is absent below 20 observations per system',
                'working file blocks and per-task Git metadata are separate; total database/server CPU/RAM and storage not measured',
                'CLI/node process startup included; no persistent worker daemon',
                'read correctness checked; no untrusted code executed',
                'not a full cost, durability or throughput comparison']}
        if args.cow_driver:
            lower = ({mount / 'source' / 'image' for mount in source_volumes} if source_volumes else
                     {root / f'.treeharbor-cow-{storage}' / 'image' for _, storage, _ in nodes.values()})
            report['setup']['treeharbor_lower_image_count'] = len(lower)
            report['setup']['treeharbor_lower_allocated_bytes'] = sum(allocated(path, exclude_git=False) for path in lower)
            report['treeharbor_source_placement'] = 'dedicated-ext4' if source_volumes else 'node-parent-filesystem'
        if args.cow_shared_node:
            report['limitations'].append('One node per format prepares concurrently; files are read after the whole batch reply and cleanup runs serially on that node')
    finally:
        if server is not None:
            server.terminate()
            server.wait(timeout=15)
        if log is not None:
            log.close()
        if created:
            sql(f'DROP SCHEMA {schema} CASCADE')
with OUT.open('x') as report_file:
    report_file.write(json.dumps(report,indent=2)+'\n')
print(json.dumps(report['sample_medians'],indent=2))
