"""Installer contract tests; all destinations and hook files are isolated."""
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

SOURCE = Path(__file__).resolve().parents[1]
FILES = ('notifier.py', '保存授权码.ps1', 'settings.example.json', 'Install.ps1',
         'install_notifier.py', 'AGENTS.md', 'START_HERE.md', 'README.md',
         'Codex邮件通知器使用与迁移说明.md', 'tests/test_notifier.py',
         'tests/test_install_notifier.py')
EVENTS = ('UserPromptSubmit', 'Stop', 'Interrupt', 'SessionEnd')


class InstallerTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix='notifier-install-test-')
        self.addCleanup(self.temp.cleanup)
        self.base = Path(self.temp.name)
        self.package = self.base / 'package'
        self.target = self.base / '通知 工具 (新)'
        self.home = self.base / 'codex home'
        self.legacy_python = self.base / 'Old Python' / 'python.exe'
        self.legacy_script = self.base / 'CodexNotifier' / 'notifier.py'
        for name in FILES:
            path = self.package / name
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_bytes(b'# fixture\n')
        (self.package / 'settings.example.json').write_text(json.dumps({
            'mode': 'auto', 'smtp_host': '', 'sender': '', 'recipient': '',
            'smtp_port': 465, 'smtp_security': 'ssl'}), encoding='utf-8')
        self.seal()

    def seal(self):
        data = {'schema_version': 1, 'files': {
            name: hashlib.sha256((self.package / name).read_bytes()).hexdigest()
            for name in FILES}}
        (self.package / 'manifest.sha256.json').write_text(json.dumps(data), encoding='utf-8')

    def install(self, **kwargs):
        spec = importlib.util.spec_from_file_location('installer', SOURCE / 'install_notifier.py')
        self.assertIsNotNone(spec)
        self.assertTrue((SOURCE / 'install_notifier.py').is_file(), 'Installer implementation is missing')
        module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(module)
        return module.install(self.package, self.target, Path(sys.executable), self.home, **kwargs)

    def hooks(self):
        return json.loads((self.home / 'hooks.json').read_text(encoding='utf-8'))

    def write_hooks(self, value):
        self.home.mkdir(parents=True, exist_ok=True)
        (self.home / 'hooks.json').write_text(json.dumps(value), encoding='utf-8')

    def test_first_install_and_repeat_preserve_bytes_and_mtimes(self):
        report = self.install()
        self.assertIn(str(self.target / 'notifier.py'), report['changed'])
        self.assertEqual(self.hooks()['hooks'].keys(), set(EVENTS))
        for event in EVENTS:
            hook = self.hooks()['hooks'][event][0]['hooks'][0]
            self.assertEqual(hook['type'], 'command')
            self.assertIn('"' + str(self.target / 'notifier.py') + '" hook', hook['command'])
        self.assertEqual(json.loads((self.target / 'settings.json').read_text())['smtp_host'], '')
        before = {str(p): (p.read_bytes(), p.stat().st_mtime_ns) for p in self.base.rglob('*') if p.is_file()}
        again = self.install()
        self.assertEqual(again['changed'], [])
        self.assertEqual(again['backups'], [])
        after = {str(p): (p.read_bytes(), p.stat().st_mtime_ns) for p in self.base.rglob('*') if p.is_file()}
        self.assertEqual(before, after)

    def test_dry_run_does_not_create_destination_or_home(self):
        report = self.install(dry_run=True)
        self.assertTrue(report['dry_run'])
        self.assertTrue(report['changed'])
        self.assertFalse(self.target.exists())
        self.assertFalse(self.home.exists())

    def test_minimum_version_rejects_python_311_before_any_writes(self):
        with patch.object(sys, 'version_info', (3, 11, 9)):
            with self.assertRaisesRegex(ValueError, 'Python 3.12'):
                self.install()
        self.assertFalse(self.target.exists())
        self.assertFalse(self.home.exists())

    def test_destination_junction_rejected_before_any_writes(self):
        actual = self.base / 'actual target'
        actual.mkdir()
        junction = self.base / 'junction target'
        result = subprocess.run(['cmd.exe', '/d', '/c', 'mklink', '/J',
                                 str(junction), str(actual)], capture_output=True,
                                text=True, errors='replace', timeout=15)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertTrue(junction.is_junction())
        self.addCleanup(junction.rmdir)
        self.target = junction / 'CodexNotifier'
        with self.assertRaisesRegex(ValueError, 'Redirected destination'):
            self.install()
        self.assertEqual(list(actual.iterdir()), [])
        self.assertFalse(self.home.exists())

    def test_preserves_settings_credential_other_hooks_and_top_level_fields(self):
        self.target.mkdir()
        settings = b'{"mode":"off","sender":"keep@example.test"}'
        credential = b'opaque-user-specific-encrypted-data'
        (self.target / 'settings.json').write_bytes(settings)
        (self.target / 'smtp.credential.xml').write_bytes(credential)
        other = {'hooks': {'Stop': [{'matcher': 'foo', 'hooks': [{'type': 'command', 'command': 'keep-me'}]}]},
                 'metadata': {'plugin': 'respire'}}
        self.write_hooks(other)
        report = self.install()
        self.assertEqual((self.target / 'settings.json').read_bytes(), settings)
        self.assertEqual((self.target / 'smtp.credential.xml').read_bytes(), credential)
        self.assertEqual(self.hooks()['hooks']['Stop'][0], other['hooks']['Stop'][0])
        self.assertEqual(self.hooks()['metadata'], other['metadata'])
        self.assertEqual(len(report['backups']), 1)

    def test_standard_old_hooks_replaced_and_unknown_command_preserved(self):
        old = f'"{self.legacy_python}" "{self.legacy_script}" hook'
        unknown = 'powershell -File custom.ps1 notifier.py'
        hooks = {event: [{'hooks': [{'type': 'command', 'command': old, 'timeout': 20},
                                   {'type': 'command', 'command': unknown}]}] for event in EVENTS}
        self.write_hooks({'hooks': hooks})
        report = self.install()
        for event in EVENTS:
            commands = [h['command'] for entry in self.hooks()['hooks'][event] for h in entry['hooks']]
            self.assertEqual(len(commands), 2)
            self.assertNotIn(old, commands)
            self.assertIn(unknown, commands)
        self.assertTrue(report['warnings'])

    def test_corrupt_package_rejected_without_writes(self):
        (self.package / 'notifier.py').write_text('corrupt', encoding='utf-8')
        with self.assertRaises(ValueError):
            self.install()
        self.assertFalse(self.target.exists())
        self.assertFalse(self.home.exists())

    def test_manifest_extra_file_rejected_without_writes(self):
        path = self.package / 'manifest.sha256.json'
        value = json.loads(path.read_text())
        value['files']['../evil.py'] = '0' * 64
        path.write_text(json.dumps(value))
        with self.assertRaises(ValueError):
            self.install()
        self.assertFalse(self.target.exists())

    def test_invalid_hook_json_and_shapes_rejected_without_writes(self):
        self.home.mkdir()
        path = self.home / 'hooks.json'
        for raw in ('{broken', '[]', '{"hooks":[]}', '{"hooks":{"Stop":{}}}',
                    '{"hooks":{"Stop":[{"hooks":"bad"}]}}',
                    '{"hooks":{"Stop":[{"hooks":[{"type":"command","command":null}]}]}}'):
            with self.subTest(raw=raw):
                path.write_text(raw)
                with self.assertRaises(ValueError):
                    self.install()
                self.assertEqual(path.read_text(), raw)
                self.assertFalse(self.target.exists())
                self.assertEqual(list(self.home.iterdir()), [path])

    def test_existing_tool_backup_contains_previous_bytes(self):
        self.target.mkdir()
        (self.target / 'notifier.py').write_bytes(b'old notifier')
        report = self.install()
        self.assertEqual(len(report['backups']), 1)
        self.assertEqual(Path(report['backups'][0]).read_bytes(), b'old notifier')
        self.assertEqual((self.target / 'notifier.py').read_bytes(), b'# fixture\n')

    def test_incomplete_package_rejected_without_writes(self):
        (self.package / 'README.md').unlink()
        with self.assertRaises(ValueError):
            self.install()
        self.assertFalse(self.target.exists())

    def test_settings_template_with_secret_or_nonempty_mail_rejected(self):
        for value in ({'mode': 'auto', 'sender': 'private@example.test'},
                      {'mode': 'auto', 'password': 'secret'}):
            (self.package / 'settings.example.json').write_text(json.dumps(value))
            self.seal()
            with self.assertRaises(ValueError):
                self.install()
            self.assertFalse(self.target.exists())


    def test_destination_parent_collision_prevents_all_writes(self):
        self.target.mkdir()
        (self.target / 'tests').write_bytes(b'keep-file')
        with self.assertRaises(ValueError):
            self.install()
        self.assertEqual(list(self.target.iterdir()), [self.target / 'tests'])
        self.assertFalse(self.home.exists())

    def test_duplicate_standard_hooks_collapsed_without_dropping_neighbors(self):
        old = subprocess.list2cmdline([str(self.base / 'python.exe'), str(self.legacy_script), 'hook'])
        group = {'hooks': [{'type': 'command', 'command': old}]}
        self.write_hooks({'hooks': {'Stop': [group, group, {'hooks': [{'type': 'command', 'command': 'keep'}]}]}})
        self.install()
        commands = [hook['command'] for group in self.hooks()['hooks']['Stop'] for hook in group['hooks']]
        self.assertEqual(len(commands), 2)
        self.assertEqual(commands[-1], 'keep')

    def test_shell_wrapper_and_similar_script_are_never_removed(self):
        other_script = self.base / 'Other' / 'notifier.py'
        commands = [f'python "{other_script}" hook',
                    f'cmd /c python "{self.legacy_script}" hook',
                    f'python "{self.legacy_script}" hook && echo extra',
                    f'python "{self.legacy_script}" hook --unknown ignored']
        self.write_hooks({'hooks': {'Stop': [{'hooks': [
            {'type': 'command', 'command': command} for command in commands]}]}})
        self.install()
        actual = [hook['command'] for group in self.hooks()['hooks']['Stop'] for hook in group['hooks']]
        self.assertEqual(actual[:4], commands)
        self.assertEqual(len(actual), 5)

    def test_bootstrap_dry_run_executes_real_python_without_side_effects(self):
        self.assertTrue((SOURCE / 'Install.ps1').is_file(), 'Bootstrap implementation is missing')
        for name in ('Install.ps1', 'install_notifier.py'):
            (self.package / name).write_bytes((SOURCE / name).read_bytes())
        self.seal()
        result = subprocess.run([
            'powershell.exe', '-NoProfile', '-NonInteractive', '-ExecutionPolicy', 'Bypass',
            '-File', str(self.package / 'Install.ps1'), '-PythonPath', sys.executable,
            '-TargetDir', str(self.target), '-CodexHome', str(self.home), '-DryRun'],
            capture_output=True, text=True, encoding='utf-8', errors='replace', timeout=45)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        report = json.loads(result.stdout)
        self.assertTrue(report['dry_run'])
        self.assertEqual(Path(report['codex_home']), self.home)
        self.assertFalse(self.target.exists())
        self.assertFalse(self.home.exists())

    def test_bootstrap_rejects_explicit_invalid_python_without_installing(self):
        self.assertTrue((SOURCE / 'Install.ps1').is_file(), 'Bootstrap implementation is missing')
        for name in ('Install.ps1', 'install_notifier.py'):
            (self.package / name).write_bytes((SOURCE / name).read_bytes())
        self.seal()
        result = subprocess.run([
            'powershell.exe', '-NoProfile', '-NonInteractive', '-ExecutionPolicy', 'Bypass',
            '-File', str(self.package / 'Install.ps1'), '-PythonPath', str(self.base / 'missing.exe'),
            '-TargetDir', str(self.target), '-CodexHome', str(self.home), '-DryRun'],
            capture_output=True, text=True, encoding='utf-8', errors='replace', timeout=45)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn('Explicit -PythonPath', result.stderr)
        self.assertFalse(self.target.exists())
        self.assertFalse(self.home.exists())

    def test_empty_existing_settings_is_rejected_before_writes(self):
        self.target.mkdir()
        (self.target / 'settings.json').write_bytes(b'')
        with self.assertRaises(ValueError):
            self.install()
        self.assertEqual(list(self.target.iterdir()), [self.target / 'settings.json'])
        self.assertFalse(self.home.exists())

    def test_generated_hook_command_round_trips_space_and_chinese_arguments(self):
        (self.package / 'notifier.py').write_text('import json,sys; print(json.dumps(sys.argv[1:]))', encoding='utf-8')
        self.seal()
        self.install()
        command = self.hooks()['hooks']['Stop'][0]['hooks'][0]['command']
        result = subprocess.run(command, shell=True, capture_output=True, text=True, timeout=15)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(json.loads(result.stdout), ['hook', '--codex-home', str(self.home)])

    def test_cli_uses_environment_home_and_explicit_home_wins(self):
        (self.package / 'install_notifier.py').write_bytes((SOURCE / 'install_notifier.py').read_bytes())
        self.seal()
        environment = dict(os.environ, CODEX_HOME=str(self.home))
        base_command = [sys.executable, str(self.package / 'install_notifier.py'),
                        '--target-dir', str(self.target), '--dry-run']
        result = subprocess.run(base_command, capture_output=True, text=True, encoding='utf-8', env=environment, timeout=30)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(json.loads(result.stdout)['codex_home'], str(self.home))
        override = self.base / 'explicit home'
        result = subprocess.run(base_command + ['--codex-home', str(override)], capture_output=True,
                                text=True, encoding='utf-8', env=environment, timeout=30)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(json.loads(result.stdout)['codex_home'], str(override))
        self.assertFalse(self.target.exists())
        self.assertFalse(self.home.exists())
        self.assertFalse(override.exists())

    def test_bootstrap_shortcut_is_local_and_repeat_is_unchanged(self):
        for name in ('Install.ps1', 'install_notifier.py'):
            (self.package / name).write_bytes((SOURCE / name).read_bytes())
        self.seal()
        command = ['powershell.exe', '-NoProfile', '-NonInteractive', '-ExecutionPolicy', 'Bypass',
                   '-File', str(self.package / 'Install.ps1'), '-PythonPath', sys.executable,
                   '-TargetDir', str(self.target), '-CodexHome', str(self.home), '-CreateShortcut']
        result = subprocess.run(command, capture_output=True, text=True, encoding='utf-8', errors='replace', timeout=45)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        link = self.target / '通知设置.lnk'
        self.assertTrue(link.is_file())
        before = (link.read_bytes(), link.stat().st_mtime_ns)
        result = subprocess.run(command, capture_output=True, text=True, encoding='utf-8', errors='replace', timeout=45)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertEqual(json.loads(result.stdout)['changed'], [])
        self.assertEqual(json.loads(result.stdout)['backups'], [])
        self.assertEqual(before, (link.read_bytes(), link.stat().st_mtime_ns))

if __name__ == '__main__':
    unittest.main()
