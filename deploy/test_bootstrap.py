"""Check secret persistence and refusal paths without contacting PostgreSQL."""
import contextlib
import io
from pathlib import Path
import stat
import subprocess
import tempfile
import unittest
from unittest.mock import patch

import configure_database as database


class BootstrapTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.config = Path(temporary.name) / 'production.env'
        self.enterContext(patch.object(database, 'CONFIG', self.config))
        self.enterContext(patch.object(database.os, 'geteuid', return_value=0))
        self.enterContext(contextlib.redirect_stdout(io.StringIO()))
        self.login = self.enterContext(patch.object(
            database.subprocess, 'run',
            return_value=subprocess.CompletedProcess([], 0, b'1', b''),
        ))

    def test_retry_keeps_all_secrets_and_restricts_file_permissions(self):
        def sql_result(sql):
            if sql.startswith('SELECT EXISTS'):
                return 'f'
            if 'pg_get_userbyid' in sql:
                return database.ROLE
            return ''

        with patch.object(database, 'postgres', side_effect=sql_result):
            database.configure()
            original = self.config.read_bytes()
            database.configure()
        self.assertEqual(self.config.read_bytes(), original)
        self.assertEqual(stat.S_IMODE(self.config.stat().st_mode), 0o600)
        self.assertEqual(self.login.call_count, 2)

    def test_existing_database_without_secret_file_is_not_taken_over(self):
        with patch.object(database, 'postgres', return_value='t'):
            with self.assertRaisesRegex(SystemExit, 'refusing to take it over'):
                database.configure()
        self.assertFalse(self.config.exists())
        self.login.assert_not_called()

    def test_config_symlink_is_rejected(self):
        original = self.config.with_name('other.env')
        original.write_text('unchanged')
        self.config.symlink_to(original)
        with self.assertRaisesRegex(SystemExit, 'Refusing a symlink'):
            database.configure()
        self.assertEqual(original.read_text(), 'unchanged')
        self.login.assert_not_called()


if __name__ == '__main__':
    unittest.main()
