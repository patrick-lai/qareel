import argparse
import fcntl
import hashlib
import json
import os
from pathlib import Path
import platform
import re
import select
import shlex
import shutil
import stat
import subprocess
import sys
import tempfile
import time
import uuid


ROOT = Path(os.path.abspath(__file__)).parents[1]
PACKAGES = ('wpe-webkit-2.0', 'wpe-platform-headless-2.0', 'json-glib-1.0', 'gio-unix-2.0', 'cairo', 'pangocairo')
SOURCES = ('qareel-browser.c', 'qareel-recording.c')
MAX_BINARY = 256 * 1024 * 1024
ENV = {'PATH': '/usr/local/bin:/usr/bin:/bin', 'LANG': 'C.UTF-8', 'LC_ALL': 'C.UTF-8'}
IMAGE_INPUTS = ('host-linux',)
IMAGE_TOOLS = {'ffmpeg': '/usr/bin/ffmpeg', 'pulseaudio': '/usr/bin/pulseaudio', 'dbus_daemon': '/usr/bin/dbus-daemon'}
IMAGE_HOST = '/usr/local/bin/qareel-browser'
IMAGE_ID = re.compile(r'sha256:[0-9a-f]{64}')
BUILD_TIMEOUT = 3600


def run(arguments, extra=None, timeout=120):
    result = subprocess.run(arguments, env=ENV | (extra or {}), stdin=subprocess.DEVNULL,
                            capture_output=True, text=True, timeout=timeout)
    if result.returncode:
        raise ValueError(f'{Path(arguments[0]).name} failed: {(result.stderr.strip() or result.stdout.strip())[-4000:]}')
    return result.stdout


def tool(value):
    resolved = shutil.which(value, path=ENV['PATH'])
    if not resolved:
        raise ValueError(f'Required tool unavailable: {value}')
    return str(Path(resolved).resolve())


def require_linux():
    if platform.system() != 'Linux':
        raise ValueError('The native WPE host must be built and installed on Linux.')


def digest(path):
    info = path.stat()
    if not stat.S_ISREG(info.st_mode) or not 0 < info.st_size <= MAX_BINARY:
        raise ValueError('Native host must be a regular file no larger than 256 MiB.')
    with path.open('rb') as stream:
        header = stream.read(20)
        expected = {'x86_64': 62, 'aarch64': 183}.get(platform.machine())
        if (expected is None or len(header) != 20 or header[:7] != b'\x7fELF\x02\x01\x01'
                or int.from_bytes(header[18:20], 'little') != expected
                or int.from_bytes(header[16:18], 'little') not in (2, 3)):
            raise ValueError('Native host must be a 64-bit Linux ELF executable for this architecture.')
        stream.seek(0)
        return hashlib.file_digest(stream, 'sha256').hexdigest()


def synced_directory(path):
    descriptor = os.open(path, os.O_RDONLY | os.O_DIRECTORY)
    try:
        os.fsync(descriptor)
    finally:
        os.close(descriptor)


def atomic_bytes(path, data, mode):
    descriptor, temporary = tempfile.mkstemp(prefix=f'.{path.name}.', dir=path.parent)
    try:
        with os.fdopen(descriptor, 'wb') as stream:
            stream.write(data)
            stream.flush()
            os.fchmod(stream.fileno(), mode)
            os.fsync(stream.fileno())
        os.replace(temporary, path)
        synced_directory(path.parent)
    finally:
        if os.path.exists(temporary):
            os.unlink(temporary)


def dependencies(pkg_config, package_path=None):
    extra = {'PKG_CONFIG_PATH': package_path} if package_path else None
    try:
        run([pkg_config, '--atleast-version=2.54', 'wpe-webkit-2.0'], extra)
        run([pkg_config, '--exists', *PACKAGES], extra)
    except ValueError as error:
        raise ValueError('Install system development packages for WPE WebKit >=2.54 with its headless platform, json-glib, gio-unix, cairo and pangocairo. '
                         'This tool does not download dependencies or mix distribution repositories.') from error
    return extra


def build(args):
    require_linux()
    compiler = tool(args.cc)
    pkg_config = tool(args.pkg_config)
    extra = dependencies(pkg_config, args.pkg_config_path)
    flags = shlex.split(run([pkg_config, '--cflags', '--libs', *PACKAGES], extra))
    output = Path(args.output).expanduser().absolute()
    output.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix='.native-webkit-', dir=output.parent) as temporary:
        binary = Path(temporary) / 'qareel-browser'
        run([compiler, '-std=c11', '-D_POSIX_C_SOURCE=200809L', '-O2', '-Wall', '-Wextra', '-Werror',
             *(str(ROOT / 'host-linux' / name) for name in SOURCES), '-o', str(binary), *flags, '-lm'])
        checksum = digest(binary)
        os.chmod(binary, 0o700)
        with binary.open('rb') as stream:
            os.fsync(stream.fileno())
        os.replace(binary, output)
        synced_directory(output.parent)
    return {'executable': str(output), 'sha256': checksum}


def loader_check(binary):
    output = run([tool('ldd'), str(binary)])
    if 'not found' in output:
        raise ValueError('Native runtime libraries are missing from the system loader. Install matching system packages; LD_LIBRARY_PATH is not inherited by the daemon.')


def ffmpeg_check(value):
    if not Path(value).is_absolute():
        raise ValueError('ffmpeg must be an absolute executable path.')
    executable = tool(value)
    if not has_libx264(run([executable, '-hide_banner', '-encoders'])):
        raise ValueError('Selected ffmpeg does not provide the libx264 encoder.')
    return executable


def has_libx264(encoders):
    return any(len(parts := line.split()) >= 2 and parts[1] == 'libx264' for line in encoders.splitlines())


def audio_tool_check(value, name):
    if not Path(value).is_absolute():
        raise ValueError(f'{name} must be an absolute executable path.')
    executable = tool(value)
    run([executable, '--version'])
    return executable


def install(args):
    require_linux()
    source = Path(args.binary).expanduser().resolve(strict=True)
    checksum = digest(source)
    loader_check(source)
    ffmpeg = ffmpeg_check(args.ffmpeg) if args.ffmpeg else None
    pulseaudio = audio_tool_check(args.pulseaudio, 'pulseaudio') if args.pulseaudio else None
    dbus_daemon = audio_tool_check(args.dbus_daemon, 'dbus_daemon') if args.dbus_daemon else None
    directory = data_directory(args.data_dir)
    target = directory / 'bin' / checksum / 'qareel-browser'
    config = {'executable': str(target), 'sha256': checksum}
    for name, value in [('ffmpeg', ffmpeg), ('pulseaudio', pulseaudio), ('dbus_daemon', dbus_daemon)]:
        if value:
            config[name] = value

    def stage():
        target.parent.mkdir(parents=True, exist_ok=True, mode=0o700)
        if target.exists() or target.is_symlink():
            if target.is_symlink() or not os.access(target, os.X_OK) or digest(target) != checksum:
                raise ValueError('Installed native binary does not match its immutable SHA directory; refusing to overwrite it.')
        else:
            descriptor, temporary = tempfile.mkstemp(prefix='.install-', dir=target.parent)
            try:
                with source.open('rb') as incoming, os.fdopen(descriptor, 'wb') as outgoing:
                    shutil.copyfileobj(incoming, outgoing, 1024 * 1024)
                    outgoing.flush()
                    os.fchmod(outgoing.fileno(), 0o700)
                    os.fsync(outgoing.fileno())
                if digest(Path(temporary)) != checksum:
                    raise ValueError('Source native binary changed during installation; configuration was not replaced.')
                os.replace(temporary, target)
                synced_directory(target.parent)
            finally:
                if os.path.exists(temporary):
                    os.unlink(temporary)
        synced_directory(target.parent.parent)

    return publish(directory, config, args.replace, stage)


def data_directory(value):
    directory = Path(value).expanduser().absolute() / 'native-webkit'
    directory.mkdir(parents=True, exist_ok=True, mode=0o700)
    return directory


def publish(directory, config, replace, stage=None):
    config_path = directory / 'runtime.json'
    with (directory / '.install.lock').open('a+b') as lock:
        os.chmod(lock.name, 0o600)
        try:
            fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError as error:
            raise ValueError('Another native runtime installation is in progress; retry after it completes.') from error
        prior = None
        current = None
        if config_path.exists() or config_path.is_symlink():
            if config_path.is_symlink() or not stat.S_ISREG(config_path.stat().st_mode) or config_path.stat().st_size > 16384:
                raise ValueError('Existing runtime configuration is not a bounded regular file; leave it untouched and repair it explicitly.')
            prior = config_path.read_bytes()
            try:
                current = json.loads(prior)
            except (ValueError, UnicodeError):
                current = None
            if current != config and not replace:
                raise ValueError('Existing runtime configuration differs; use --replace to replace it while retaining its backup and previously installed runtimes.')
        if stage:
            stage()
        encoded = (json.dumps(config, sort_keys=True, indent=2) + '\n').encode()
        if prior is not None and current == config:
            return config
        if prior is not None:
            backup = directory / f'runtime.previous.{hashlib.sha256(prior).hexdigest()}.json'
            atomic_bytes(backup, prior, 0o600)
        atomic_bytes(config_path, encoded, 0o600)
    return config


def engine_environment():
    home = os.environ.get('HOME', '')
    if not os.path.isabs(home):
        raise ValueError("HOME must be an absolute path so podman uses this user's image store.")
    extra = {'HOME': home}
    runtime = os.environ.get('XDG_RUNTIME_DIR', '')
    if os.path.isabs(runtime):
        extra['XDG_RUNTIME_DIR'] = runtime
    return extra


def build_context(destination):
    for name in IMAGE_INPUTS:
        source = ROOT / name
        target = destination / name
        target.parent.mkdir(parents=True, exist_ok=True)
        if source.is_dir():
            shutil.copytree(source, target, symlinks=False)
        else:
            shutil.copy2(source, target)


def image_check(engine, image_id, extra):
    base = [engine, 'run', '--rm', '--pull=never', '--network=none', '--userns=keep-id', f'--user={os.getuid()}:{os.getgid()}', '--entrypoint']
    if 'not found' in run([*base, '/usr/bin/ldd', image_id, IMAGE_HOST], extra):
        raise ValueError('The browser image is missing native libraries for its host; runtime configuration was not changed.')
    if not has_libx264(run([*base, IMAGE_TOOLS['ffmpeg'], image_id, '-hide_banner', '-encoders'], extra)):
        raise ValueError('The browser image ffmpeg does not provide the libx264 encoder; runtime configuration was not changed.')
    for name in ('pulseaudio', 'dbus_daemon'):
        run([*base, IMAGE_TOOLS[name], image_id, '--version'], extra)


def image(args):
    require_linux()
    if os.geteuid() == 0:
        raise ValueError('Build the browser image as the user that runs qareel; rootless podman keeps one image store per user.')
    engine = tool(args.engine)
    extra = engine_environment()
    version = run([engine, 'version', '--format', '{{.Client.Version}}'], extra).strip()
    major = version.split('.')[0]
    if not major.isdigit() or int(major) < 4:
        raise ValueError(f'Rootless podman 4 or newer is required; found {version or "an unknown version"}.')
    directory = data_directory(args.data_dir)
    with tempfile.TemporaryDirectory(prefix='qareel-browser-image-') as temporary:
        context = Path(temporary) / 'context'
        build_context(context)
        identifier = Path(temporary) / 'image-id'
        refresh = ['--pull=newer', '--no-cache'] if args.update else []
        run([engine, 'build', '--file', str(context / 'host-linux/Containerfile'), '--tag', args.tag,
             '--iidfile', str(identifier), *refresh, str(context)], extra, BUILD_TIMEOUT)
        image_id = identifier.read_text().strip()
    if not IMAGE_ID.fullmatch(image_id) or run([engine, 'image', 'inspect', '--format', '{{.Id}}', image_id], extra).strip() != image_id.removeprefix('sha256:'):
        raise ValueError('podman did not report the built image id consistently; runtime configuration was not changed.')
    image_check(engine, image_id, extra)
    return publish(directory, {'container': {'engine': engine, 'image': image_id}, **IMAGE_TOOLS}, args.replace)


USERLAND_SUITE = 'testing'
USERLAND_MIRROR = 'http://deb.debian.org/debian'
USERLAND_KEYRING = '/usr/share/keyrings/debian-archive-keyring.gpg'
USERLAND_ARCHITECTURES = {'x86_64': ('amd64', 'x86_64-linux-gnu', 'ld-linux-x86-64.so.2'), 'aarch64': ('arm64', 'aarch64-linux-gnu', 'ld-linux-aarch64.so.1')}
USERLAND_SKIPPED = ('systemd',)
USERLAND_BASE = ('proot', 'dpkg', 'perl-base', 'debconf', 'bash', 'dash', 'coreutils', 'sed', 'grep', 'findutils', 'util-linux', 'tar', 'diffutils', 'libc-bin', 'libglib2.0-bin')
USERLAND_RUNTIME = ('libwpewebkit-2.0-1', 'libjson-glib-1.0-0', 'libcairo2', 'libpango-1.0-0', 'libpangocairo-1.0-0', 'ca-certificates', 'glib-networking',
                    'fonts-dejavu-core', 'fonts-noto-core', 'fonts-noto-cjk', 'fonts-noto-color-emoji', 'gstreamer1.0-plugins-base', 'gstreamer1.0-plugins-good',
                    'gstreamer1.0-libav', 'libegl1', 'libgles2', 'libgl1-mesa-dri', 'libgbm1', 'ffmpeg', 'pulseaudio', 'dbus-daemon', 'python3', 'python3-pil', 'python3-numpy')
USERLAND_BUILD = ('gcc', 'libc6-dev', 'pkg-config', 'python3', 'libwpewebkit-2.0-dev', 'libjson-glib-dev', 'libcairo2-dev', 'libpango1.0-dev')
USERLAND_BINDS = ('/proc', '/dev', '/sys')
USERLAND_OPTIONAL_BINDS = ('/etc/passwd', '/etc/group', '/etc/resolv.conf', '/etc/hosts')
USERLAND_GUEST_ENVIRONMENT = ('PATH=/usr/local/bin:/usr/bin:/bin', 'LANG=C.UTF-8', 'WEBKIT_DISABLE_SANDBOX_THIS_IS_DANGEROUS=1')
USERLAND_PROXY_VARIABLES = ('http_proxy', 'https_proxy', 'no_proxy', 'HTTP_PROXY', 'HTTPS_PROXY', 'NO_PROXY')
USERLAND_ARTIFACTS = ('usr/share/glib-2.0/schemas/gschemas.compiled', 'etc/ssl/certs/ca-certificates.crt', 'etc/ld.so.cache', 'usr/bin/proot', 'usr/bin/ffmpeg')
USERLAND_HOST = 'usr/local/bin/qareel-browser'
USERLAND_SMOKE_SECONDS = 90


def say(message):
    print(message, file=sys.stderr, flush=True)


def userland_architecture():
    architecture = USERLAND_ARCHITECTURES.get(platform.machine())
    if architecture is None:
        raise ValueError(f'The userland browser runtime supports x86-64 and AArch64 Linux; found {platform.machine() or "an unknown machine"}.')
    return architecture


def userland_proot_environment(scratch):
    return {'PROOT_NO_SECCOMP': '1', 'PROOT_TMP_DIR': str(scratch)}


def userland_command(root, architecture, binds, guest_environment, program, working='/', root_user=False):
    library = Path(root) / 'usr/lib' / architecture[1]
    command = [str(library / architecture[2]), '--library-path', str(library), str(Path(root) / 'usr/bin/proot')]
    if root_user:
        command.append('-0')
    command += ['--kill-on-exit', '-r', str(root)]
    for bind in binds:
        command += ['-b', bind]
    return [*command, '-w', working, '/usr/bin/env', '-i', *guest_environment, *program]


def userland_binds(profile=None):
    binds = [*USERLAND_BINDS, *(path for path in USERLAND_OPTIONAL_BINDS if os.path.isfile(path))]
    if profile is not None:
        binds.append(f'{profile}:{profile}')
    return binds


def inside(root, architecture, scratch, program, binds=None, working='/', root_user=False, timeout=1800):
    guest = ('PATH=/usr/sbin:/usr/bin:/sbin:/bin', 'HOME=/root', 'DEBIAN_FRONTEND=noninteractive', 'LANG=C.UTF-8')
    command = userland_command(root, architecture, binds if binds is not None else USERLAND_BINDS, guest, program, working, root_user)
    return run(command, userland_proot_environment(scratch), timeout)


def userland_apt(work, cache, architecture, mirror, suite, keyring):
    for relative in ('state/lists/partial', f'cache/{cache}/archives/partial'):
        (work / relative).mkdir(parents=True, exist_ok=True)
    (work / 'status').write_text('')
    (work / 'sources.list').write_text(f'deb [signed-by={keyring} arch={architecture[0]}] {mirror} {suite} main\n')
    settings = {'Dir::State': work / 'state', 'Dir::State::status': work / 'status', 'Dir::Cache': work / 'cache' / cache,
                'Dir::Etc::sourcelist': work / 'sources.list', 'Dir::Etc::sourceparts': '/nonexistent', 'Dir::Etc::preferences': work / 'preferences',
                'Dir::Etc::preferencesparts': '/nonexistent', 'APT::Architecture': architecture[0], 'APT::Install-Recommends': 'false',
                'APT::Sandbox::User': '', 'Debug::NoLocking': 'true', 'Acquire::Languages': 'none'}
    configuration = work / f'apt-{cache}.conf'
    configuration.write_text(''.join(f'{key} "{value}";\n' for key, value in settings.items()))
    environment = {'APT_CONFIG': str(configuration)}
    environment.update({name: os.environ[name] for name in USERLAND_PROXY_VARIABLES if name in os.environ})
    return environment


def userland_download(environment, packages, timeout=BUILD_TIMEOUT):
    run([tool('apt-get'), 'install', '-y', '--download-only', '--no-install-recommends', *packages], environment, timeout)


def deb_identity(path):
    name, version, _ = Path(path).name.removesuffix('.deb').split('_', 2)
    return name, version.replace('%3a', ':')


def archive_packages(archives):
    return sorted(Path(archives).glob('*.deb'), key=lambda path: path.name)


def untar(deb, root):
    producer = subprocess.Popen([tool('dpkg-deb'), '--fsys-tarfile', str(deb)], stdout=subprocess.PIPE, stderr=subprocess.PIPE, env=ENV)
    consumer = subprocess.run([tool('tar'), '-x', '-C', str(root), '--keep-directory-symlink', '--no-same-owner', '--no-same-permissions'],
                              stdin=producer.stdout, capture_output=True, env=ENV)
    producer.stdout.close()
    errors = producer.stderr.read()
    if producer.wait() or consumer.returncode:
        raise ValueError(f'Could not unpack {Path(deb).name}: {(errors or consumer.stderr).decode(errors="replace")[-1000:]}')


def userland_extract(debs, root):
    for directory in ('usr/bin', 'usr/lib', 'usr/sbin', 'tmp', 'proc', 'dev', 'sys', 'etc', 'root', 'archives', 'var/lib/dpkg/info', 'var/lib/dpkg/updates', 'var/cache'):
        (root / directory).mkdir(parents=True, exist_ok=True)
    for link, target in (('bin', 'usr/bin'), ('lib', 'usr/lib'), ('sbin', 'usr/sbin')):
        os.symlink(target, root / link)
    (root / 'var/lib/dpkg/status').write_text('')
    (root / 'var/lib/dpkg/available').write_text('')
    (root / 'etc/passwd').write_text('root:x:0:0:root:/root:/bin/sh\n')
    (root / 'etc/group').write_text('root:x:0:\n')
    (root / 'etc/nsswitch.conf').write_text('passwd: files\ngroup: files\nhosts: files dns\n')
    (root / 'tmp').chmod(0o1777)
    kept = [deb for deb in debs if deb_identity(deb)[0] not in USERLAND_SKIPPED]
    for deb in kept:
        shutil.copy2(deb, root / 'archives' / Path(deb).name)
        untar(deb, root)


def userland_configure(root, architecture, scratch):
    inside(root, architecture, scratch, ['/usr/bin/dpkg', '--force-all', '--no-triggers', '--unpack', '--recursive', '/archives'], root_user=True)
    try:
        inside(root, architecture, scratch, ['/usr/bin/dpkg', '--force-all', '--configure', '--pending'], root_user=True)
    except ValueError as error:
        say(f'Some package configuration scripts failed; checking that the runtime is usable: {str(error)[-300:]}')
    shutil.rmtree(root / 'archives', ignore_errors=True)
    missing = [relative for relative in USERLAND_ARTIFACTS if not (root / relative).exists()]
    if missing:
        raise ValueError(f'The userland root filesystem is incomplete; missing {", ".join(missing)}.')


def userland_root(work, cache, architecture, scratch):
    root = work / f'root-{cache}'
    userland_extract(archive_packages(work / 'cache' / cache / 'archives'), root)
    userland_configure(root, architecture, scratch)
    return root


def userland_compile(builder, architecture, scratch, work):
    sources = work / 'sources'
    for name in IMAGE_INPUTS:
        source = ROOT / name
        target = sources / name
        target.parent.mkdir(parents=True, exist_ok=True)
        if source.is_dir():
            shutil.copytree(source, target, symlinks=False)
        else:
            shutil.copy2(source, target)
    output = work / 'out'
    output.mkdir()
    inside(builder, architecture, scratch, ['/usr/bin/python3', 'host-linux/native_webkit.py', 'build', '--output', '/out/qareel-browser'],
           binds=[*USERLAND_BINDS, f'{sources}:/src', f'{output}:/out'], working='/src')
    return output / 'qareel-browser'


def userland_checks(root, architecture, scratch):
    host = '/' + USERLAND_HOST
    if 'not found' in inside(root, architecture, scratch, ['/usr/bin/ldd', host]):
        raise ValueError('The userland root filesystem is missing native libraries for its host; runtime configuration was not changed.')
    if not has_libx264(inside(root, architecture, scratch, [IMAGE_TOOLS['ffmpeg'], '-hide_banner', '-encoders'])):
        raise ValueError('The userland root filesystem ffmpeg does not provide the libx264 encoder; runtime configuration was not changed.')
    for name in ('pulseaudio', 'dbus_daemon'):
        inside(root, architecture, scratch, [IMAGE_TOOLS[name], '--version'])


def userland_launch(root, architecture, scratch, profile):
    environment = [*USERLAND_GUEST_ENVIRONMENT, f'HOME={profile}']
    command = userland_command(root, architecture, userland_binds(profile), environment, ['/' + USERLAND_HOST])
    return subprocess.Popen(command, env=ENV | userland_proot_environment(scratch), stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL)


def userland_smoke(root, architecture, scratch):
    profile = scratch / 'smoke-profile'
    profile.mkdir(mode=0o700)
    process = userland_launch(root, architecture, scratch, profile)
    try:
        bootstrap = {'profile_dir': str(profile), 'profile_id': str(uuid.uuid4()), **IMAGE_TOOLS}
        process.stdin.write((json.dumps(bootstrap) + '\n' + json.dumps({'type': 'hello', 'generation': 'g1', 'version': 1}) + '\n').encode())
        process.stdin.flush()
        deadline = time.monotonic() + USERLAND_SMOKE_SECONDS
        pending = b''
        while time.monotonic() < deadline:
            ready, _, _ = select.select([process.stdout], [], [], 1)
            if ready:
                chunk = os.read(process.stdout.fileno(), 65536)
                if not chunk:
                    break
                pending += chunk
            for line in pending.split(b'\n')[:-1]:
                try:
                    message = json.loads(line)
                except ValueError:
                    continue
                if message.get('type') == 'ready':
                    operations = message.get('capabilities', {}).get('operations', [])
                    if not {'recording_video', 'recording_audio_app'} <= set(operations):
                        raise ValueError('The userland browser host started but does not advertise video and app-audio recording; runtime configuration was not changed.')
                    return operations
            pending = pending.rpartition(b'\n')[2]
        raise ValueError('The userland browser host did not become ready; runtime configuration was not changed.')
    finally:
        process.terminate()
        try:
            process.wait(timeout=10)
        except subprocess.TimeoutExpired:
            process.kill()
            process.wait()


def userland_manifest(architecture, mirror, suite, debs, host_digest):
    packages = {}
    for deb in debs:
        name, version = deb_identity(deb)
        packages[name] = {'version': version, 'sha256': digest_file(deb)}
    return {'architecture': architecture[0], 'mirror': mirror, 'suite': suite, 'packages': packages, 'host_sha256': host_digest}


def digest_file(path):
    with Path(path).open('rb') as stream:
        return hashlib.file_digest(stream, 'sha256').hexdigest()


def userland_prune(directory, keep):
    for entry in directory.iterdir():
        if entry.name != keep and not entry.name.startswith('.build-') and entry.is_dir():
            shutil.rmtree(entry, ignore_errors=True)


def userland(args):
    require_linux()
    if os.geteuid() == 0:
        raise ValueError('Build the userland browser runtime as the user that runs qareel.')
    architecture = userland_architecture()
    keyring = args.keyring
    if not Path(keyring).is_file():
        raise ValueError(f'The Debian archive keyring is missing at {keyring}; install it (for example `sudo apt-get install debian-archive-keyring`) or pass --keyring.')
    directory = data_directory(args.data_dir)
    base = directory / 'userland'
    base.mkdir(mode=0o700, exist_ok=True)
    work = Path(tempfile.mkdtemp(prefix='.build-', dir=base))
    scratch = work / 'proot'
    scratch.mkdir()
    try:
        say('Resolving the Debian packages for the browser runtime')
        runtime_environment = userland_apt(work, 'runtime', architecture, args.mirror, args.suite, keyring)
        run([tool('apt-get'), 'update'], runtime_environment, BUILD_TIMEOUT)
        say('Downloading the browser runtime packages')
        userland_download(runtime_environment, (*USERLAND_BASE, *USERLAND_RUNTIME))
        build_environment = userland_apt(work, 'build', architecture, args.mirror, args.suite, keyring)
        for deb in archive_packages(work / 'cache/runtime/archives'):
            shutil.copy2(deb, work / 'cache/build/archives' / deb.name)
        say('Downloading the compiler packages')
        userland_download(build_environment, (*USERLAND_BASE, *USERLAND_RUNTIME, *USERLAND_BUILD))
        debs = archive_packages(work / 'cache/runtime/archives')
        say('Unpacking and configuring the compiler root filesystem')
        builder = userland_root(work, 'build', architecture, scratch)
        say('Compiling the browser host')
        host = userland_compile(builder, architecture, scratch, work)
        shutil.rmtree(builder)
        say('Unpacking and configuring the browser root filesystem')
        root = userland_root(work, 'runtime', architecture, scratch)
        target = root / USERLAND_HOST
        target.parent.mkdir(parents=True, exist_ok=True)
        shutil.copy2(host, target)
        target.chmod(0o755)
        encoded = (json.dumps(userland_manifest(architecture, args.mirror, args.suite, debs, digest_file(host)), sort_keys=True, indent=2) + '\n').encode()
        identity = hashlib.sha256(encoded).hexdigest()
        say('Checking the browser root filesystem')
        userland_checks(root, architecture, scratch)
        final = base / identity[:16]
        if not final.exists():
            staging = Path(tempfile.mkdtemp(prefix='.stage-', dir=base))
            os.rename(root, staging / 'rootfs')
            (staging / 'manifest.json').write_bytes(encoded)
            os.rename(staging, final)
        say('Starting the browser host once')
        userland_smoke(final / 'rootfs', architecture, scratch)
        config = {'userland': {'rootfs': str(final / 'rootfs'), 'manifest_sha256': identity}, **IMAGE_TOOLS}
        published = publish(directory, config, args.replace)
        if args.replace:
            userland_prune(base, final.name)
        return published
    finally:
        shutil.rmtree(work, ignore_errors=True)


def main():
    parser = argparse.ArgumentParser(description='Build or install the native Linux WPE host, directly, as a pinned rootless podman image, or as a userland root filesystem.')
    commands = parser.add_subparsers(dest='command', required=True)
    builder = commands.add_parser('build')
    builder.add_argument('--cc', default='cc')
    builder.add_argument('--pkg-config', default='pkg-config')
    builder.add_argument('--pkg-config-path')
    builder.add_argument('--output', default=str(ROOT / '.build/native-linux/qareel-browser'))
    installer = commands.add_parser('install')
    installer.add_argument('--binary', required=True)
    installer.add_argument('--data-dir', default=str(Path.home() / '.qareel'))
    installer.add_argument('--ffmpeg')
    installer.add_argument('--pulseaudio')
    installer.add_argument('--dbus-daemon')
    installer.add_argument('--replace', action='store_true')
    imager = commands.add_parser('image')
    imager.add_argument('--engine', default='podman')
    imager.add_argument('--tag', default='localhost/qareel-browser:runtime')
    imager.add_argument('--data-dir', default=str(Path.home() / '.qareel'))
    imager.add_argument('--update', action='store_true')
    imager.add_argument('--replace', action='store_true')
    rooter = commands.add_parser('userland')
    rooter.add_argument('--data-dir', default=str(Path.home() / '.qareel'))
    rooter.add_argument('--mirror', default=USERLAND_MIRROR)
    rooter.add_argument('--suite', default=USERLAND_SUITE)
    rooter.add_argument('--keyring', default=USERLAND_KEYRING)
    rooter.add_argument('--replace', action='store_true')
    args = parser.parse_args()
    try:
        print(json.dumps({'build': build, 'install': install, 'image': image, 'userland': userland}[args.command](args)))
    except (ValueError, OSError, subprocess.SubprocessError) as error:
        print(str(error), file=sys.stderr)
        return 1
    return 0


if __name__ == '__main__':
    raise SystemExit(main())
