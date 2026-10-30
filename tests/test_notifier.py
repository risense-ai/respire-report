"""Isolated notification regressions; never read credentials or send email."""
import importlib.util
from contextlib import closing
import json
import os
import sqlite3
import subprocess
import sys
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch


SOURCE = Path(__file__).resolve().parents[1] / 'notifier.py'
spec = importlib.util.spec_from_file_location('notifier_under_test', SOURCE)
notifier = importlib.util.module_from_spec(spec)
spec.loader.exec_module(notifier)
REAL_ROLLOUT_PATH = notifier.rollout_path


class NotificationTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.root = Path(self.tmp.name)
        self.c = notifier.DEFAULT | {'mode': 'always', 'quiet_enabled': False,
                                     'include_question': False}
        (self.root / 'settings.json').write_text(json.dumps(self.c), encoding='utf-8')
        for target, value in [('ROOT', self.root), ('rollout_path', lambda *args: None)]:
            self.enterContext(patch.object(notifier, target, value))
        self.enterContext(patch.object(notifier, 'send', side_effect=AssertionError('Real email forbidden')))
        self.enterContext(patch.object(notifier, 'start_background', side_effect=AssertionError('Background process forbidden')))
        self.enterContext(patch.dict(os.environ, {'PLUGIN_ROOT': '', 'PLUGIN_DATA': '',
                                                'CODEX_HOME': str(self.root / 'codex')}))

    def respire_environment(self):
        home = self.root / 'codex'
        plugin = home / 'plugins' / 'cache' / 'rsrsai' / 'respire' / '0.2.8'
        manifest = plugin / '.codex-plugin' / 'plugin.json'
        manifest.parent.mkdir(parents=True, exist_ok=True)
        manifest.write_text('{"name":"respire","version":"0.2.8"}', encoding='utf-8')
        data = home / 'plugins' / 'data' / 'respire-rsrsai'
        data.mkdir(parents=True, exist_ok=True)
        return {'PLUGIN_ROOT': str(plugin), 'PLUGIN_DATA': str(data)}

    def event(self, name='Stop', message='完成', **fields):
        return {'hook_event_name': name, 'session_id': 'test-session',
                'turn_id': 'test-turn', 'last_assistant_message': message,
                'transcript_path': '', **fields}

    def rows(self, sql='SELECT status,message FROM events'):
        with notifier.database() as db:
            return db.execute(sql).fetchall()

    def test_normal_json_is_queued_unchanged(self):
        text = '{"result":"stored","ids":["d93fd35a-c8d6-53fd-8792-6290ec7ec01c"]}'
        self.assertEqual(notifier.receive(self.event(message=text), self.c), 'pending')
        self.assertEqual(self.rows(), [('pending', text)])

    def test_regular_json_and_unnamed_session_are_not_filtered(self):
        text = '{"value":42,"ok":true}'
        self.assertEqual(notifier.receive(self.event(message=text), self.c), 'pending')
        self.assertEqual(self.rows(), [('pending', text)])

    def test_normal_submission_still_monitored(self):
        self.assertEqual(notifier.receive(self.event('UserPromptSubmit'), self.c), 'watching')
        self.assertEqual(self.rows('SELECT status FROM turns'), [('active',)])

    def test_normal_interrupt_and_duplicate(self):
        event = self.event('Interrupt')
        self.assertEqual(notifier.receive(event, self.c), 'pending')
        self.assertEqual(notifier.receive(event, self.c), 'duplicate')
        self.assertEqual(self.rows(), [('pending', '')])

    def test_normal_mode_and_allowlist_rules_remain(self):
        for index, config in enumerate((self.c | {'mode': 'off'},
                                       self.c | {'mode': 'auto'},
                                       self.c | {'sessions': ['another-session']})):
            with self.subTest(config=config['mode'], index=index):
                event = self.event(turn_id=f'turn-{index}')
                self.assertEqual(notifier.receive(event, config, is_away=False), 'suppressed')
        self.assertTrue(all(row == ('suppressed', '') for row in self.rows()))

    def test_quiet_hours_still_override_always(self):
        with patch.object(notifier, 'quiet_hours', return_value=True):
            self.assertEqual(notifier.receive(self.event(), self.c), 'suppressed')
        self.assertEqual(self.rows(), [('suppressed', '')])

    def test_respire_success_receipts_never_enter_send_queue(self):
        env = self.respire_environment()
        for result in ('stored', 'already-stored', 'nothing-to-store', 'read-only'):
            text = json.dumps({'result': result, 'ids': ['d93fd35a-c8d6-53fd-8792-6290ec7ec01c']})
            with self.subTest(result=result), patch.dict(os.environ, env):
                event = self.event(message=text, turn_id=result)
                self.assertEqual(notifier.receive(event, self.c), 'suppressed_respire')
        self.assertEqual(self.rows(), [('suppressed_respire', '')] * 4)

    def test_respire_submission_does_not_start_stalled_monitor(self):
        with patch.dict(os.environ, self.respire_environment()):
            self.assertEqual(notifier.receive(self.event('UserPromptSubmit'), self.c), 'ignored_respire_background')
        self.assertEqual(self.rows('SELECT status FROM turns'), [])

    def test_respire_success_closes_monitor_created_by_old_version(self):
        self.assertEqual(notifier.receive(self.event('UserPromptSubmit'), self.c), 'watching')
        with patch.dict(os.environ, self.respire_environment()):
            self.assertEqual(notifier.receive(self.event(message='{"result":"nothing-to-store","ids":[]}'), self.c), 'suppressed_respire')
        self.assertEqual(self.rows('SELECT status FROM turns'), [('completed',)])

    def test_respire_interrupt_and_unknown_or_error_results_remain(self):
        messages = ['{"result":"failed","ids":[]}', '{"ok":true}', '保存失败',
                    '{"result":"stored","ids":[false]}',
                    '{"result":"stored","ids":[],"error":"unconfirmed"}',
                    '[]', 'null', '{"result":"stored","ids":["not-an-id"]}']
        with patch.dict(os.environ, self.respire_environment()):
            for index, message in enumerate(messages):
                with self.subTest(message=message):
                    self.assertEqual(notifier.receive(self.event(message=message, turn_id=str(index)), self.c), 'pending')
            self.assertEqual(notifier.receive(self.event('Interrupt', turn_id='interrupt'), self.c), 'pending')
        self.assertEqual(len(self.rows()), len(messages) + 1)
        self.assertTrue(all(row[0] == 'pending' for row in self.rows()))

    def test_partial_unrelated_or_broken_source_markers_do_not_filter(self):
        env = self.respire_environment()
        variants = [env | {'PLUGIN_ROOT': ''}, env | {'PLUGIN_DATA': ''},
                    env | {'PLUGIN_DATA': env['PLUGIN_DATA'] + '-other'},
                    env | {'PLUGIN_ROOT': env['PLUGIN_ROOT'] + '/child'},
                    env | {'PLUGIN_ROOT': str(self.root / 'respire') }]
        for index, markers in enumerate(variants):
            with self.subTest(index=index), patch.dict(os.environ, markers):
                event = self.event(message='{"result":"nothing-to-store","ids":[]}', turn_id=str(index))
                self.assertEqual(notifier.receive(event, self.c), 'pending')
        manifest = Path(env['PLUGIN_ROOT']) / '.codex-plugin' / 'plugin.json'
        manifest.write_text('{broken', encoding='utf-8')
        with patch.dict(os.environ, env):
            self.assertEqual(notifier.receive(self.event(message='{"result":"nothing-to-store","ids":[]}', turn_id='broken'), self.c), 'pending')

    def test_shiwu_subagents_keep_normal_notifications(self):
        for role in ('shiwu_research', 'shiwu_execute', 'shiwu_verify', 'shiwu_accept'):
            with self.subTest(role=role):
                event = self.event(message='{"result":"stored","ids":[]}',
                                   session_id=role, agent_role=role, source='subagent')
                self.assertEqual(notifier.receive(event, self.c), 'pending')

    def test_manifest_identity_and_version_must_match(self):
        env = self.respire_environment()
        manifest = Path(env['PLUGIN_ROOT']) / '.codex-plugin' / 'plugin.json'
        for index, body in enumerate(('{}', '[]', '{"name":"other","version":"0.2.8"}',
                                      '{"name":"respire","version":"0.2.9"}')):
            manifest.write_text(body, encoding='utf-8')
            with self.subTest(body=body), patch.dict(os.environ, env):
                event = self.event(message='{"result":"stored","ids":[]}', turn_id=str(index))
                self.assertEqual(notifier.receive(event, self.c), 'pending')
        manifest.unlink()
        with patch.dict(os.environ, env):
            self.assertEqual(notifier.receive(self.event(message='{"result":"stored","ids":[]}'), self.c), 'pending')

    def test_sender_delivers_normal_json_but_never_filtered_receipt(self):
        text = '{"result":"nothing-to-store","ids":[]}'
        self.assertEqual(notifier.receive(self.event(message=text, turn_id='normal'), self.c), 'pending')
        with patch.dict(os.environ, self.respire_environment()):
            self.assertEqual(notifier.receive(self.event(message=text, turn_id='background'), self.c), 'suppressed_respire')
        delivered = []
        with patch.object(notifier, 'send', side_effect=lambda row, config: delivered.append(row)):
            notifier.worker()
        self.assertEqual([(row[6], row[5]) for row in delivered], [('normal', text)])
        self.assertEqual(self.rows(), [('sent', ''), ('suppressed_respire', '')])

    def test_filter_does_not_depend_on_body_or_question_email_settings(self):
        with patch.dict(os.environ, self.respire_environment()):
            event = self.event(message='{"result":"nothing-to-store","ids":[]}')
            self.assertEqual(notifier.receive(event, self.c | {'include_message': False, 'include_question': True}), 'suppressed_respire')
            self.assertEqual(notifier.receive(event, self.c), 'duplicate')
        self.assertEqual(self.rows(), [('suppressed_respire', '')])

    def test_invalid_already_stored_receipt_is_not_a_success(self):
        with patch.dict(os.environ, self.respire_environment()):
            event = self.event(message='{"result":"already-stored","ids":[]}')
            self.assertEqual(notifier.receive(event, self.c), 'pending')

    def test_excessively_nested_json_keeps_notification(self):
        with patch.dict(os.environ, self.respire_environment()):
            event = self.event(message='[' * 10000 + '0' + ']' * 10000)
            self.assertEqual(notifier.receive(event, self.c), 'pending')

    def test_duplicate_receipt_fields_keep_notification(self):
        with patch.dict(os.environ, self.respire_environment()):
            event = self.event(message='{"result":"failed","result":"stored","ids":[]}')
            self.assertEqual(notifier.receive(event, self.c), 'pending')

    def test_custom_codex_home_resolves_transcript_and_title(self):
        home = self.root / 'codex'
        sessions = home / 'sessions'
        sessions.mkdir(parents=True)
        transcript = sessions / 'rollout.jsonl'
        transcript.write_text('', encoding='utf-8')
        with closing(sqlite3.connect(home / 'state_5.sqlite')) as db:
            db.execute('CREATE TABLE threads(id,rollout_path,name,title,cwd,project_id)')
            db.execute('CREATE TABLE projects(id,name)')
            db.execute('CREATE TABLE project_roots(project_id,path)')
            db.execute('INSERT INTO threads VALUES(?,?,?,?,?,?)',
                       ('custom-session', str(transcript), '迁移验证', '', str(home / 'demo'), None))
            db.commit()
        fake_default = self.root / 'other-profile'
        fake_default.mkdir()
        with patch.object(Path, 'home', return_value=fake_default):
            self.assertEqual(REAL_ROLLOUT_PATH('custom-session', {}), transcript.resolve())
            self.assertEqual(REAL_ROLLOUT_PATH('custom-session', {'transcript_path': str(transcript)}), transcript.resolve())
            self.assertEqual(notifier.thread_display('custom-session'), ('demo', '迁移验证'))

    def test_hook_cli_uses_explicit_codex_home(self):
        markers = self.respire_environment()
        script = self.root / 'notifier.py'
        script.write_bytes(SOURCE.read_bytes())
        (self.root / 'settings.json').write_text(json.dumps(self.c | {'mode': 'off'}), encoding='utf-8')
        event = self.event(message='{"result":"nothing-to-store","ids":[]}')
        child_env = os.environ | markers | {'CODEX_HOME': str(self.root / 'wrong-home')}
        result = subprocess.run([sys.executable, '-B', str(script), 'hook', '--codex-home', str(self.root / 'codex')],
                                input=json.dumps(event), text=True, encoding='utf-8',
                                capture_output=True, env=child_env, timeout=10)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(self.rows(), [('suppressed_respire', '')])

    def extraction(self):
        return {'facts': [{'text': '已完成隔离验证', 'status': 'confirmed',
                           'sources': ['item-1-part-0']}],
                'constraints': [{'text': '保留其他会话通知', 'sources': ['item-0-part-0']}]}

    def test_respire_extraction_is_silent_and_diagnostics_do_not_store_content(self):
        text = json.dumps(self.extraction(), ensure_ascii=False)
        with patch.dict(os.environ, self.respire_environment()):
            self.assertEqual(notifier.receive(self.event(message=text), self.c), 'suppressed_respire')
        self.assertEqual(self.rows(), [('suppressed_respire', '')])
        log = (self.root / 'hook-diagnostics.jsonl').read_text(encoding='utf-8')
        records = [json.loads(line) for line in log.splitlines()]
        self.assertTrue(any(row.get('output_kind') == 'extraction' for row in records))
        self.assertNotIn('已完成隔离验证', log)
        self.assertNotIn('item-1-part-0', log)

    def test_empty_respire_extraction_is_silent(self):
        with patch.dict(os.environ, self.respire_environment()):
            self.assertEqual(notifier.receive(self.event(message='{"facts":[],"constraints":[]}'), self.c), 'suppressed_respire')

    def test_ordinary_and_shiwu_extraction_shaped_json_is_delivered(self):
        text = json.dumps(self.extraction(), ensure_ascii=False)
        for sid in ('ordinary', 'shiwu_research', 'shiwu_execute', 'shiwu_verify', 'shiwu_accept'):
            with self.subTest(sid=sid):
                self.assertEqual(notifier.receive(self.event(message=text, session_id=sid), self.c), 'pending')
        delivered = []
        with patch.object(notifier, 'send', side_effect=lambda row, config: delivered.append(row)):
            notifier.worker()
        self.assertEqual(len(delivered), 5)
        self.assertTrue(all(row[5] == text for row in delivered))

    def test_invalid_extraction_shapes_are_not_silenced(self):
        valid = self.extraction()
        cases = [valid | {'error': 'failed'}, {'facts': [], 'constraints': None},
                 {'facts': valid['facts']}, {'facts': valid['facts'] * 25, 'constraints': []}]
        for change in ({'status': 'failed'}, {'text': ' '}, {'sources': []},
                       {'sources': ['unrecognized-source']}, {'sources': [True]},
                       {'error': 'unconfirmed'}, {'text': 'x' * 1501}, {'text': '😀' * 751}):
            cases.append({'facts': [valid['facts'][0] | change], 'constraints': []})
        cases.append({'facts': [], 'constraints': [valid['constraints'][0] | {'status': 'confirmed'}]})
        with patch.dict(os.environ, self.respire_environment()):
            for index, case in enumerate(cases):
                with self.subTest(index=index):
                    self.assertEqual(notifier.receive(self.event(message=json.dumps(case, ensure_ascii=False), turn_id=str(index)), self.c), 'pending')

    def test_extraction_total_size_and_duplicate_keys_are_not_silenced(self):
        text = json.dumps({'facts': [], 'constraints': [{'text': 'x' * 1500, 'sources': ['item-0-part-0']}] * 4})
        duplicate = '{"facts":[{"error":"failed"}],"facts":[],"constraints":[]}'
        with patch.dict(os.environ, self.respire_environment()):
            self.assertEqual(notifier.receive(self.event(message=text), self.c), 'pending')
            self.assertEqual(notifier.receive(self.event(message=duplicate, turn_id='duplicate-fields'), self.c), 'pending')

    def test_real_hook_entry_filters_extraction_without_model_or_smtp(self):
        markers = self.respire_environment()
        script = self.root / 'notifier.py'
        script.write_bytes(SOURCE.read_bytes())
        (self.root / 'settings.json').write_text(json.dumps(self.c | {'mode': 'off'}), encoding='utf-8')
        result = subprocess.run([sys.executable, '-B', str(script), 'hook', '--codex-home', str(self.root / 'codex')],
                                input=json.dumps(self.event(message=json.dumps(self.extraction()))),
                                text=True, encoding='utf-8', capture_output=True,
                                env=os.environ | markers, timeout=10)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(self.rows(), [('suppressed_respire', '')])

    def test_extraction_empty_text_uses_ecmascript_trim_rules(self):
        cases = [('\ufeff', 'pending'), ('\u00a0\u2028', 'pending'),
                 ('\x85', 'suppressed_respire'), ('\x1c', 'suppressed_respire'),
                 ('\u180e', 'suppressed_respire')]
        with patch.dict(os.environ, self.respire_environment()):
            for index, (text, expected) in enumerate(cases):
                payload = {'facts': [{'text': text, 'status': 'confirmed', 'sources': ['item-1-part-0']}],
                           'constraints': []}
                with self.subTest(index=index):
                    self.assertEqual(notifier.receive(self.event(message=json.dumps(payload), turn_id=str(index)), self.c), expected)


if __name__ == '__main__':
    unittest.main(verbosity=2)
