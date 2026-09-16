"""Exercise release switching with real files and simulated systemd/HTTP, without SSH."""
import hashlib
import os
from pathlib import Path
import subprocess
import tempfile
import unittest

SCRIPT = Path(__file__).with_name('activate.sh')
OLD = 'a' * 40 + '-1-1'
NEW = 'b' * 40 + '-2-1'

# Systemd is not present on developer Macs. GNU mv/sha256sum/flock are also
# represented here by their equivalent filesystem/hash/lock operations.
COMMAND = r'''#!/usr/bin/env python3
import fcntl
import hashlib
import os
from pathlib import Path
import sys

name = Path(sys.argv[0]).name
args = sys.argv[1:]
base = Path(os.environ['DEPLOY_ROOT'])
current = base / 'current'
release = current.resolve().name if current.is_symlink() else ''
if name == 'sudo':
    assert args[0] == '-n' and args[1] == '/usr/bin/systemctl'
    os.execv(str(Path(sys.argv[0]).with_name('systemctl')), ['systemctl', *args[2:]])
elif name == 'systemctl':
    with (base / 'commands.log').open('a') as log:
        log.write(' '.join(args) + ' ' + release + '\n')
    assert args[-1] == 'proxy-microservice.service'
    if args[0] == 'restart':
        if release == os.environ.get('FAIL_RESTART'):
            sys.exit(1)
        (base / 'running').write_text(release)
    elif args[0] == 'stop':
        (base / 'running').unlink(missing_ok=True)
    elif args[0] == 'is-active':
        sys.exit(0 if (base / 'running').exists() else 3)
    else:
        assert args[0] in {'cat', 'reset-failed'}
elif name == 'curl':
    if release in os.environ.get('FAIL_HEALTH', '').split(','):
        sys.exit(22)
    print('ok')
elif name == 'sleep':
    pass
elif name == 'flock':
    fcntl.flock(int(args[-1]), fcntl.LOCK_EX)
elif name == 'mv':
    assert args[0] == '-Tf'
    os.replace(args[1], args[2])
elif name == 'sha256sum':
    expected, filename = sys.stdin.read().strip().split('  ', 1)
    sys.exit(0 if hashlib.sha256(Path(filename).read_bytes()).hexdigest() == expected else 1)
else:
    raise AssertionError(name)
'''


class ReleaseTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.bin = self.root / 'commands'
        self.bin.mkdir()
        for name in ['sudo', 'systemctl', 'curl', 'sleep', 'flock', 'mv', 'sha256sum']:
            command = self.bin / name
            command.write_text(COMMAND)
            command.chmod(0o755)
        (self.root / 'releases').mkdir()
        self.incoming = self.root / 'incoming' / NEW
        self.incoming.mkdir(parents=True)
        binary = b'a simulated immutable release binary\n'
        (self.incoming / 'proxy-microservice').write_bytes(binary)
        (self.incoming / 'SHA256SUMS').write_text(
            hashlib.sha256(binary).hexdigest() + '  proxy-microservice\n'
        )

    def previous_release(self):
        previous = self.root / 'releases' / OLD
        previous.mkdir()
        (previous / 'proxy-microservice').write_text('previous binary')
        (previous / 'proxy-microservice').chmod(0o755)
        (self.root / 'current').symlink_to('releases/' + OLD)
        (self.root / 'running').write_text(OLD)

    def deploy(self, release=NEW, **extra):
        return subprocess.run(
            ['bash', str(SCRIPT), release], text=True, capture_output=True,
            env={**os.environ, 'DEPLOY_ROOT': str(self.root),
                 'PATH': str(self.bin) + os.pathsep + os.environ['PATH'], **extra},
            timeout=30,
        )

    def test_success_switches_binary_and_cleans_upload(self):
        self.previous_release()
        result = self.deploy()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(os.readlink(self.root / 'current'), 'releases/' + NEW)
        self.assertTrue(os.access(self.root / 'current' / 'proxy-microservice', os.X_OK))
        self.assertFalse(self.incoming.exists())
        self.assertTrue((self.root / 'releases' / OLD).is_dir())

    def test_checksum_failure_never_restarts_or_switches(self):
        self.previous_release()
        (self.incoming / 'proxy-microservice').write_text('damaged upload')
        result = self.deploy()
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(os.readlink(self.root / 'current'), 'releases/' + OLD)
        self.assertFalse((self.root / 'commands.log').exists())

    def test_unhealthy_release_restores_previous_and_reports_failure(self):
        self.previous_release()
        result = self.deploy(FAIL_HEALTH=NEW)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn('Previous release restored', result.stderr)
        self.assertEqual(os.readlink(self.root / 'current'), 'releases/' + OLD)
        self.assertEqual((self.root / 'running').read_text(), OLD)

    def test_restart_error_also_rolls_back(self):
        self.previous_release()
        result = self.deploy(FAIL_RESTART=NEW)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn('Previous release restored', result.stderr)
        self.assertEqual(os.readlink(self.root / 'current'), 'releases/' + OLD)

    def test_failed_first_release_stops_service(self):
        result = self.deploy(FAIL_HEALTH=NEW)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn('First release failed', result.stderr)
        self.assertFalse((self.root / 'current').is_symlink())
        self.assertFalse((self.root / 'running').exists())

    def test_failed_rollback_is_not_reported_as_success(self):
        self.previous_release()
        result = self.deploy(FAIL_HEALTH=NEW + ',' + OLD)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn('Activation AND rollback failed', result.stderr)
        self.assertNotIn('Previous release restored', result.stderr)

    def test_invalid_release_path_is_rejected_without_side_effects(self):
        result = self.deploy(release='../../arbitrary')
        self.assertEqual(result.returncode, 2)
        self.assertFalse((self.root / 'commands.log').exists())
        self.assertTrue(self.incoming.exists())


if __name__ == '__main__':
    unittest.main()
