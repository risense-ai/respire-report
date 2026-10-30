"""Verified, idempotent Windows installer. Never imports notifier or sends mail.

The manifest covers the fixed distributable files; runtime settings, DPAPI
credentials, databases and logs are deliberately outside the copy allowlist.
"""
import argparse
import copy
from datetime import datetime, timezone
import hashlib
import json
import os
from pathlib import Path, PureWindowsPath
import re
import subprocess
import sys
import tempfile
import uuid

PACKAGE_FILES = (
    'notifier.py', '保存授权码.ps1', 'settings.example.json', 'Install.ps1',
    'install_notifier.py', 'AGENTS.md', 'START_HERE.md', 'README.md',
    'Codex邮件通知器使用与迁移说明.md', 'tests/test_notifier.py',
    'tests/test_install_notifier.py',
)
MANIFEST = 'manifest.sha256.json'
EVENTS = ('UserPromptSubmit', 'Stop', 'Interrupt', 'SessionEnd')
TEMPLATE_KEYS = {'mode', 'idle_seconds', 'sessions', 'smtp_host', 'smtp_port',
                 'smtp_security', 'sender', 'recipient', 'include_message',
                 'retention_days', 'stalled_seconds', 'quiet_enabled',
                 'quiet_start', 'quiet_end', 'include_question'}
PYTHON_PROBE = (
    'import sys, tkinter, sqlite3, ssl, ctypes, json; '
    'assert sys.version_info >= (3, 12), "Python 3.12+ required"; '
    'assert sys.platform == "win32", "Windows required"; '
    'print(json.dumps({"executable":sys.executable,"version":list(sys.version_info[:3])}))'
)


def read_json(data, label):
    """Reject malformed JSON and duplicate keys instead of silently dropping data."""
    def unique_pairs(pairs):
        result = {}
        for key, value in pairs:
            if key in result:
                raise ValueError(f'{label}: duplicate key {key}')
            result[key] = value
        return result
    try:
        return json.loads(data.decode('utf-8-sig'), object_pairs_hook=unique_pairs,
                          parse_constant=lambda _: (_ for _ in ()).throw(ValueError(f'{label}: invalid number')))
    except (UnicodeError, json.JSONDecodeError) as exc:
        raise ValueError(f'{label}: invalid UTF-8 JSON') from exc


def package_payload(package_root):
    """Validate every fixed file before returning bytes; manifests cannot add paths."""
    # 1. 清单仅用于固定交付文件的完整性校验，不能扩大复制权限。
    manifest_path = package_root / MANIFEST
    if not manifest_path.is_file():
        raise ValueError('Package manifest is missing')
    manifest_bytes = manifest_path.read_bytes()
    manifest = read_json(manifest_bytes, MANIFEST)
    if (not isinstance(manifest, dict) or type(manifest.get('schema_version')) is not int
            or manifest['schema_version'] != 1 or not isinstance(manifest.get('files'), dict)
            or set(manifest['files']) != set(PACKAGE_FILES)):
        raise ValueError('Manifest must use schema_version 1 and the exact fixed file allowlist')
    payload = {}
    for name in PACKAGE_FILES:
        source = package_root / name
        digest = manifest['files'][name]
        if not source.is_file() or not source.resolve().is_relative_to(package_root.resolve()):
            raise ValueError(f'Package file missing or outside package: {name}')
        data = source.read_bytes()
        if not isinstance(digest, str) or not re.fullmatch('[0-9a-fA-F]{64}', digest):
            raise ValueError(f'Invalid SHA256: {name}')
        if hashlib.sha256(data).hexdigest() != digest.lower():
            raise ValueError(f'Package SHA256 mismatch: {name}')
        payload[name] = data
    # 2. 清单自身随工具保留，便于在已安装目录复核，但不参与自身哈希。
    payload[MANIFEST] = manifest_bytes
    template = read_json(payload['settings.example.json'], 'settings.example.json')
    if (not isinstance(template, dict) or set(template) - TEMPLATE_KEYS
            or template.get('mode') != 'auto' or template.get('sessions', []) != []
            or any(template.get(key) != '' for key in ('smtp_host', 'sender', 'recipient'))):
        raise ValueError('Settings template must be auto mode, empty mail fields and contain no private keys')
    return payload


def validate_python(python_path):
    """Execute a harmless import probe; file existence alone is insufficient."""
    # 安装器本身也必须提供 junction 检查，不能仅验证目标解释器版本。
    if sys.version_info < (3, 12):
        raise ValueError('Installer requires Python 3.12+ for junction-safe path validation')
    try:
        probe = subprocess.run([str(python_path), '-c', PYTHON_PROBE], capture_output=True,
                               text=True, encoding='utf-8', timeout=20, check=True)
        actual = Path(json.loads(probe.stdout)['executable']).resolve()
    except (OSError, subprocess.SubprocessError, ValueError, KeyError) as exc:
        raise ValueError('Python must run on Windows, be 3.12+, and provide tkinter/sqlite3/ssl/ctypes') from exc
    return actual


def validate_hooks(value):
    """Validate existing structure without changing unknown events or metadata."""
    if not isinstance(value, dict) or ('hooks' in value and not isinstance(value['hooks'], dict)):
        raise ValueError('hooks.json must be an object with an object-valued hooks field')
    for event, groups in value.get('hooks', {}).items():
        if not isinstance(groups, list):
            raise ValueError(f'hooks.{event} must be an array')
        for group in groups:
            if not isinstance(group, dict) or not isinstance(group.get('hooks'), list):
                raise ValueError(f'hooks.{event} groups must contain a hooks array')
            if 'matcher' in group and not isinstance(group['matcher'], str):
                raise ValueError(f'hooks.{event} matcher must be a string')
            for hook in group['hooks']:
                if not isinstance(hook, dict) or not isinstance(hook.get('type'), str):
                    raise ValueError(f'hooks.{event} hook must have a string type')
                if hook['type'] == 'command' and not isinstance(hook.get('command'), str):
                    raise ValueError(f'hooks.{event} command must be a string')


def standard_notifier_command(command, target):
    """Recognize only a direct Python notifier invocation, never a shell wrapper."""
    token = r'(?:"[^"\r\n]+"|[^\s"&|<>^]+)'
    if not re.fullmatch(rf'{token}(?:\s+{token}){{2}}(?:\s+--codex-home\s+{token})?', command.strip()):
        return False
    args = [part[1:-1] if part.startswith('"') else part
            for part in re.findall(token, command.strip())]
    python_name = PureWindowsPath(args[0]).name
    script = PureWindowsPath(args[1])
    expected = PureWindowsPath(str(target / 'notifier.py'))
    return (re.fullmatch(r'pythonw?(?:3(?:\.\d+)?)?(?:\.exe)?', python_name, re.IGNORECASE) is not None
            and args[2] == 'hook' and script.is_absolute()
            and (script == expected or (script.name.lower() == 'notifier.py'
                                       and script.parent.name.lower() == 'codexnotifier')))


def quoted_path(path):
    """Always quote paths; reject shell expansion and unsupported quote characters."""
    text = str(path)
    if any(char in text for char in ('"', '\r', '\n', '%', '!')):
        raise ValueError('Paths containing quotes, newlines, percent or exclamation marks are unsupported')
    return '"' + text + '"'


def merge_hooks(original, command, target):
    """Update recognized notifier commands once per event, preserving unrelated hooks."""
    merged = copy.deepcopy(original)
    events = merged.setdefault('hooks', {})
    warnings = []
    for event in EVENTS:
        # 1. 每个事件只保留一个无条件通知器入口；带自定义 matcher 的配置不擅自改写。
        groups = events.setdefault(event, [])
        found = False
        cleaned = []
        for group in groups:
            remaining = []
            for hook in group['hooks']:
                old_command = hook.get('command', '')
                recognized = (hook['type'] == 'command'
                              and group.get('matcher', '') in ('', '*')
                              and standard_notifier_command(old_command, target))
                if recognized:
                    if not found:
                        hook['command'] = command
                        remaining.append(hook)
                        found = True
                    continue
                # 未知形式保留原样，报告位置而不输出可能携带秘密的完整命令。
                if isinstance(old_command, str) and 'notifier.py' in old_command.lower():
                    warnings.append(f'{event}: preserved an unrecognized notifier command; review manually')
                remaining.append(hook)
            if remaining or not group['hooks']:
                group['hooks'] = remaining
                cleaned.append(group)
        # 2. 只有缺少标准入口时才添加，重复安装不产生新 Hook。
        if not found:
            cleaned.append({'hooks': [{'type': 'command', 'command': command,
                                       'timeout': 10 if event == 'Stop' else 3}]})
        events[event] = cleaned
    return merged, warnings


def validate_destination(path):
    """Reject file parents and redirected destinations before any installation writes."""
    for parent in (path, *path.parents):
        if parent.is_symlink() or parent.is_junction():
            raise ValueError(f'Redirected destination is unsupported: {parent}')
        if parent != path and parent.exists() and not parent.is_dir():
            raise ValueError(f'Destination parent is not a directory: {parent}')
    if path.exists() and not path.is_file():
        raise ValueError(f'Destination is not a regular file: {path}')


def atomic_write(path, data):
    """Replace one file atomically via a temporary sibling; never truncate in place."""
    path.parent.mkdir(parents=True, exist_ok=True)
    descriptor, temporary = tempfile.mkstemp(prefix='.notifier-install-', dir=path.parent)
    try:
        with os.fdopen(descriptor, 'wb') as output:
            output.write(data)
            output.flush()
            os.fsync(output.fileno())
        os.replace(temporary, path)
    finally:
        if os.path.exists(temporary):
            os.unlink(temporary)


def install(package_root, target_dir, python_path, codex_home, dry_run=False):
    """Preflight all bytes and destinations, then apply only changed files.

    Existing settings and DPAPI credentials are never copied from the package.
    Per-file atomic replacement is not a whole-install transaction: disk failure
    may leave partial progress; timestamped backups remain available for recovery.
    """
    # 1. 读取、验证所有输入；直到完整计划生成都不创建目录或备份。
    package_root = Path(package_root).absolute()
    target = Path(target_dir).absolute()
    home = Path(codex_home).absolute()
    payload = package_payload(package_root)
    python = validate_python(python_path)
    command = f'{quoted_path(python)} {quoted_path(target / "notifier.py")} hook --codex-home {quoted_path(home)}'
    hooks_path = home / 'hooks.json'
    validate_destination(hooks_path)
    original_hooks_bytes = hooks_path.read_bytes() if hooks_path.exists() else None
    original_hooks = read_json(original_hooks_bytes, 'hooks.json') if original_hooks_bytes is not None else {}
    validate_hooks(original_hooks)
    merged, warnings = merge_hooks(original_hooks, command, target)
    settings_path = target / 'settings.json'
    validate_destination(settings_path)
    existing_settings = settings_path.read_bytes() if settings_path.exists() else None
    settings = read_json(existing_settings if existing_settings is not None else payload['settings.example.json'], 'settings.json')
    if not isinstance(settings, dict):
        raise ValueError('Existing settings.json must be a JSON object; it was not changed')

    # 2. 按字节比较工具文件；Hook 按结构比较以保留既有格式和修改时间。
    desired = {target / name: data for name, data in payload.items()}
    if existing_settings is None:
        desired[settings_path] = payload['settings.example.json']
    desired[hooks_path] = (original_hooks_bytes if original_hooks == merged and original_hooks_bytes is not None
                           else (json.dumps(merged, ensure_ascii=False, indent=2) + '\n').encode('utf-8'))
    changes = []
    unchanged = [str(settings_path)] if existing_settings is not None else []
    for path, data in desired.items():
        validate_destination(path)
        before = path.read_bytes() if path.exists() else None
        if before == data:
            unchanged.append(str(path))
        else:
            changes.append((path, before, data))
    # SMTP 参数仅报告缺项名称，不回显用户邮箱或秘密。
    missing = [name for name in ('smtp_host', 'sender', 'recipient') if not settings.get(name)]
    if not (target / 'smtp.credential.xml').is_file():
        missing.append('smtp_authorization_code (save on this Windows user via 保存授权码.ps1)')
    report = {
        'target_dir': str(target),  # 安装位置：由显式参数或 LOCALAPPDATA 默认值解析。
        'python_path': str(python),  # 解释器：来自实际运行探针返回的 executable。
        'codex_home': str(home),  # Codex 用户目录：显式参数优先，其次环境和用户默认目录。
        'hooks_path': str(hooks_path),  # Hook 配置：只修改选定用户目录下 hooks.json。
        'dry_run': dry_run,  # 预演：true 时 changed 是计划，不代表发生写入。
        'changed': [str(path) for path, _, _ in changes],  # 变化文件：仅列当前计划涉及的精确路径。
        'unchanged': unchanged,  # 未变化文件：保留字节内容和修改时间。
        'backups': [],  # 备份：仅列本次实际保存的旧文件，不含秘密配置。
        'missing_user_information': missing,  # 待填信息：只列字段名，不暴露现有字段值。
        'warnings': warnings + ['Hook trust must be reviewed manually in Codex; no trusted_hash was written.'],
    }
    if dry_run:
        return report

    # 3. 写入前再次比较快照，避免覆盖预检期间的用户修改；每个旧文件先备份再替换。
    for path, before, _ in changes:
        current = path.read_bytes() if path.exists() else None
        if current != before:
            raise ValueError(f'File changed during preflight; retry: {path}')
    stamp = datetime.now(timezone.utc).strftime('%Y%m%dT%H%M%S%fZ') + '-' + uuid.uuid4().hex[:8]
    for path, before, data in changes:
        if before is not None:
            backup = path.with_name(path.name + '.bak-' + stamp)
            atomic_write(backup, before)
            report['backups'].append(str(backup))
        atomic_write(path, data)
    return report


def main():
    """Resolve explicit/environment/default paths and print a secret-free JSON report."""
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--target-dir', type=Path)
    parser.add_argument('--python-path', type=Path, default=Path(sys.executable))
    parser.add_argument('--codex-home', type=Path)
    parser.add_argument('--dry-run', action='store_true')
    args = parser.parse_args()
    local = os.environ.get('LOCALAPPDATA')
    if args.target_dir is None and not local:
        parser.error('LOCALAPPDATA is unset; supply --target-dir')
    target = args.target_dir if args.target_dir is not None else Path(local) / 'CodexNotifier'
    home = args.codex_home or Path(os.environ.get('CODEX_HOME') or (Path.home() / '.codex'))
    try:
        report = install(Path(__file__).resolve().parent, target, args.python_path, home, args.dry_run)
    except (OSError, ValueError) as exc:
        print(json.dumps({'error': str(exc), 'success': False}, ensure_ascii=False), file=sys.stderr)
        return 1
    print(json.dumps(report, ensure_ascii=False, indent=2))
    return 0


if __name__ == '__main__':
    if hasattr(sys.stdout, 'reconfigure'):
        sys.stdout.reconfigure(encoding='utf-8')
        sys.stderr.reconfigure(encoding='utf-8')
    raise SystemExit(main())
