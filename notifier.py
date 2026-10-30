"""Local Codex hook receiver, SMTP queue and notification settings."""
import argparse
import ctypes
from contextlib import contextmanager
import hashlib
import json
import msvcrt
import os
from pathlib import Path
import re
import smtplib
import sqlite3
import ssl
import subprocess
import sys
import time
import uuid
from email.message import EmailMessage

ROOT = Path(__file__).resolve().parent
DEFAULT = {'mode': 'auto', 'idle_seconds': 300, 'sessions': [],
           'smtp_host': '', 'smtp_port': 465, 'smtp_security': 'ssl',
           'sender': '', 'recipient': '', 'include_message': True,
           'retention_days': 30, 'stalled_seconds': 1800,
           'quiet_enabled': True, 'quiet_start': '23:00', 'quiet_end': '07:30',
           'include_question': True}

WATCHDOG_POLL_SECONDS = 15
WATCHDOG_MAX_SECONDS = 24 * 60 * 60
# 与 Respire 的 JavaScript trim() 对齐：包含 BOM，不包含 Python strip() 额外的控制空白。
ECMASCRIPT_WHITESPACE = ('\t\n\v\f\r \u00a0\u1680\u2000\u2001\u2002\u2003\u2004'
                        '\u2005\u2006\u2007\u2008\u2009\u200a\u2028\u2029\u202f\u205f\u3000\ufeff')

def safe_text(value):
    """Return SQLite/UTF-8 safe text even if a hook contains lone surrogates."""
    return str(value or '').encode('utf-8', errors='replace').decode('utf-8')

def config():
    p = ROOT / 'settings.json'
    return DEFAULT | (json.loads(p.read_text(encoding='utf-8')) if p.exists() else {})

def save(c):
    p = ROOT / 'settings.tmp'
    p.write_text(json.dumps(c, ensure_ascii=False, indent=2), encoding='utf-8')
    p.replace(ROOT / 'settings.json')

def diagnostic(stage, **fields):
    # Metadata only: never persist hook input, message content or SMTP secrets.
    record = {'time': time.strftime('%Y-%m-%d %H:%M:%S'), 'stage': stage} | fields
    with (ROOT / 'hook-diagnostics.jsonl').open('a', encoding='utf-8') as log:
        log.write(json.dumps(record, ensure_ascii=True) + '\n')

def error_category(exc):
    """Return useful, secret-free diagnostics suitable for logs and SQLite."""
    if isinstance(exc, smtplib.SMTPAuthenticationError):
        return f'smtp_auth_failed:{getattr(exc, "smtp_code", "unknown")}'
    if isinstance(exc, (smtplib.SMTPConnectError, smtplib.SMTPServerDisconnected)):
        return 'smtp_connection_failed'
    if isinstance(exc, subprocess.CalledProcessError):
        return f'credential_read_failed:{exc.returncode}'
    if isinstance(exc, json.JSONDecodeError):
        return 'invalid_json'
    if isinstance(exc, sqlite3.Error):
        return f'database_error:{type(exc).__name__}'
    if isinstance(exc, OSError):
        return f'os_error:{getattr(exc, "winerror", None) or getattr(exc, "errno", "unknown")}'
    if isinstance(exc, ValueError):
        return 'invalid_configuration'
    return type(exc).__name__

def cleanup_storage(db, c):
    """Keep message bodies transient and bound the local event history."""
    days = max(1, int(c.get('retention_days', DEFAULT['retention_days'])))
    cutoff = time.time() - days * 24 * 60 * 60
    db.execute('UPDATE events SET message="" WHERE status!="pending" AND message!=""')
    db.execute('DELETE FROM events WHERE created<?', (cutoff,))
    db.execute('DELETE FROM turns WHERE started<? AND status!="active"', (cutoff,))
    # UserPromptSubmit may fire more than once during one visible Codex turn.
    # Keep only the newest active monitor per session so one stalled conversation
    # cannot generate a batch of equivalent email notifications.
    db.execute(
        'UPDATE turns AS older SET status="superseded" '
        'WHERE older.status="active" AND EXISTS ('
        'SELECT 1 FROM turns AS newer WHERE newer.session=older.session '
        'AND newer.status="active" AND ('
        'newer.started>older.started OR '
        '(newer.started=older.started AND newer.turn>older.turn)))')

@contextmanager
def database():
    db = sqlite3.connect(ROOT / 'events.db', timeout=5)
    db.execute('CREATE TABLE IF NOT EXISTS events (id TEXT PRIMARY KEY, created REAL, session TEXT, turn TEXT DEFAULT "", event TEXT, status TEXT, attempts INTEGER DEFAULT 0, next_try REAL DEFAULT 0, error TEXT DEFAULT "", message TEXT DEFAULT "")')
    columns = {row[1] for row in db.execute('PRAGMA table_info(events)')}
    if 'turn' not in columns:
        db.execute('ALTER TABLE events ADD COLUMN turn TEXT DEFAULT ""')
    if 'message' not in columns:
        db.execute('ALTER TABLE events ADD COLUMN message TEXT DEFAULT ""')
    db.execute('CREATE TABLE IF NOT EXISTS turns (session TEXT, turn TEXT, started REAL, last_activity REAL, transcript TEXT DEFAULT "", status TEXT DEFAULT "active", notified INTEGER DEFAULT 0, PRIMARY KEY(session,turn))')
    try:
        with db:
            cleanup_storage(db, config())
            yield db
    finally:
        db.close()

def session_locked():
    class WTSINFOEX_LEVEL1(ctypes.Structure):
        _fields_ = [('SessionId', ctypes.c_ulong),
                    ('SessionState', ctypes.c_int),
                    ('SessionFlags', ctypes.c_long)]

    class WTSINFOEX_DATA(ctypes.Union):
        _fields_ = [('Level1', WTSINFOEX_LEVEL1),
                    ('Alignment', ctypes.c_longlong)]

    class WTSINFOEX(ctypes.Structure):
        _fields_ = [('Level', ctypes.c_ulong),
                    ('Data', WTSINFOEX_DATA)]

    wts = ctypes.windll.wtsapi32
    buffer = ctypes.c_void_p()
    size = ctypes.c_ulong()
    if not wts.WTSQuerySessionInformationW(0, 0xffffffff, 25,
                                           ctypes.byref(buffer), ctypes.byref(size)):
        return None
    try:
        info = ctypes.cast(buffer, ctypes.POINTER(WTSINFOEX)).contents
        if info.Level != 1:
            return None
        if info.Data.Level1.SessionFlags == 0:
            return True
        if info.Data.Level1.SessionFlags == 1:
            return False
        return None
    finally:
        wts.WTSFreeMemory(buffer)

def away(threshold):
    locked = session_locked()
    if locked is not None:
        if locked:
            return True
    else:
        # Fallback for environments where WTSInfoEx is unavailable.
        # OpenInputDesktop returns the desktop currently receiving input.
        user = ctypes.windll.user32
        user.OpenInputDesktop.restype = ctypes.c_void_p
        desk = user.OpenInputDesktop(0, False, 0x0100)
        if not desk:
            return True
        try:
            if not user.SwitchDesktop(ctypes.c_void_p(desk)):
                return True
        finally:
            user.CloseDesktop(ctypes.c_void_p(desk))

    class LASTINPUTINFO(ctypes.Structure):
        _fields_ = [('cbSize', ctypes.c_uint), ('dwTime', ctypes.c_uint)]
    user = ctypes.windll.user32
    info = LASTINPUTINFO()
    info.cbSize = ctypes.sizeof(info)
    if not user.GetLastInputInfo(ctypes.byref(info)):
        raise OSError('Cannot read Windows idle time')
    elapsed = (ctypes.windll.kernel32.GetTickCount() - info.dwTime) & 0xffffffff
    return elapsed / 1000 >= threshold

def clock_minutes(value):
    """解析本地 HH:MM 设置；拒绝越界或含糊格式。"""
    parts = str(value).split(':')
    if len(parts) != 2 or any(len(p) != 2 or not p.isdigit() for p in parts):
        raise ValueError('免打扰时间须为 HH:MM')
    hour, minute = map(int, parts)
    if hour > 23 or minute > 59:
        raise ValueError('免打扰时间超出范围')
    return hour * 60 + minute

def quiet_hours(c, now=None):
    """按电脑本地时间判断免打扰，含开始、不含结束，支持跨午夜。"""
    if not c.get('quiet_enabled', True):
        return False
    start = clock_minutes(c.get('quiet_start', '23:00'))
    end = clock_minutes(c.get('quiet_end', '07:30'))
    if start == end:
        raise ValueError('免打扰开始与结束时间不能相同')
    local = time.localtime(time.time() if now is None else now)
    minute = local.tm_hour * 60 + local.tm_min
    return start <= minute < end if start < end else minute >= start or minute < end

def session_enabled(sid, c):
    # Empty list means all sessions; a populated list is an explicit allowlist.
    return not c['sessions'] or sid in c['sessions']

def codex_home():
    """统一使用本次 Hook 指定或进程继承的 Codex 目录，避免迁移后查错会话库。"""
    return Path(os.environ.get('CODEX_HOME') or Path.home() / '.codex').expanduser().resolve()

def respire_background_source():
    """校验后台进程继承的 Respire 双路径标记；缺失或不可核实就保留通知。"""
    plugin_root = os.environ.get('PLUGIN_ROOT', '')
    plugin_data = os.environ.get('PLUGIN_DATA', '')
    if not plugin_root or not plugin_data:
        return False
    try:
        home = codex_home()
        root = Path(plugin_root).resolve()
        data = Path(plugin_data).resolve()
        # 只认当前 Codex 家目录下官方来源的版本目录，不能按名称包含 respire 猜测。
        if (root.parent != (home / 'plugins/cache/rsrsai/respire').resolve()
                or data != (home / 'plugins/data/respire-rsrsai').resolve()):
            return False
        manifest = json.loads((root / '.codex-plugin/plugin.json').read_text(encoding='utf-8'))
        return (isinstance(manifest, dict) and manifest.get('name') == 'respire'
                and manifest.get('version') == root.name)
    except (OSError, ValueError, RuntimeError):
        # 插件迁移、升级或来源标记变化时不扩大拦截范围。
        return False

def respire_extraction_shape(value, raw):
    """检查分块提炼的结构和边界；只能验证 source 格式，不能验证原始证据成员关系。"""
    # 1. 对齐插件 JavaScript 的 UTF-16 字符预算，不把长文本或其他 JSON 当作提炼结果。
    if len(raw.encode('utf-16-le', errors='surrogatepass')) // 2 > 6000:
        return False
    for key in ('facts', 'constraints'):
        entries = value[key]
        if not isinstance(entries, list) or len(entries) > 24:
            return False
        allowed = {'text', 'sources', 'status'} if key == 'facts' else {'text', 'sources'}
        # 2. 每条候选必须完整符合协议；未知字段或失败状态保留原通知，防止掩盖错误。
        for entry in entries:
            if not isinstance(entry, dict) or set(entry) != allowed:
                return False
            text = entry['text']
            if (not isinstance(text, str) or not text.strip(ECMASCRIPT_WHITESPACE)
                    or len(text.encode('utf-16-le', errors='surrogatepass')) // 2 > 1500):
                return False
            sources = entry['sources']
            if (not isinstance(sources, list) or not sources
                    or any(not isinstance(source, str)
                           or not re.fullmatch(r'item-[0-9]+-part-[0-9]+', source) for source in sources)):
                return False
            if key == 'facts' and entry['status'] not in ('confirmed', 'proposed', 'uncertain'):
                return False
    return True

def respire_output_kind(value):
    """将已确认后台来源的输出分为 receipt/extraction/unrecognized；不确认真实归档成功。"""
    if not isinstance(value, str):
        return 'unrecognized'
    def unique_fields(pairs):
        """拒绝重复字段，防止解析器用后值覆盖回执中的失败信息。"""
        fields = dict(pairs)
        if len(fields) != len(pairs):
            raise ValueError('duplicate_receipt_field')
        return fields
    try:
        receipt = json.loads(value, object_pairs_hook=unique_fields)
        # 长上下文会先逐块提炼，再生成最终保存回执；两者都是无需提醒的内部输出。
        if isinstance(receipt, dict) and set(receipt) == {'facts', 'constraints'}:
            return 'extraction' if respire_extraction_shape(receipt, value) else 'unrecognized'
        if not isinstance(receipt, dict) or set(receipt) != {'result', 'ids'}:
            return 'unrecognized'
        if receipt['result'] not in ('stored', 'already-stored', 'nothing-to-store', 'read-only'):
            return 'unrecognized'
        ids = receipt['ids']
        # Respire 的 already-stored 协议要求至少提供一条已核实记录。
        if receipt['result'] == 'already-stored' and not ids:
            return 'unrecognized'
        valid = isinstance(ids, list) and all(
            isinstance(item, str) and str(uuid.UUID(item)) == item.lower() for item in ids)
        return 'receipt' if valid else 'unrecognized'
    except (ValueError, TypeError, AttributeError, RecursionError):
        return 'unrecognized'

def track_turn(event, c):
    """Track a submitted turn without retaining the user's prompt."""
    sid = safe_text(event.get('session_id', ''))
    turn = safe_text(event.get('turn_id', ''))
    if not sid or not turn or c['mode'] == 'off' or not session_enabled(sid, c):
        return 'ignored'
    path = rollout_path(sid, event)
    now = time.time()
    activity = max(now, path.stat().st_mtime) if path else now
    with database() as db:
        db.execute('UPDATE turns SET status="superseded" '
                   'WHERE session=? AND status="active" AND turn!=?',
                   (sid, turn))
        db.execute(
            'INSERT INTO turns(session,turn,started,last_activity,transcript,status,notified) '
            'VALUES(?,?,?,?,?,"active",0) ON CONFLICT(session,turn) DO UPDATE SET '
            'started=excluded.started,last_activity=excluded.last_activity,'
            'transcript=excluded.transcript,status="active",notified=0',
            (sid, turn, now, activity, str(path) if path else ''))
    return 'watching'

def finish_turn(sid, turn, status):
    if not sid:
        return
    with database() as db:
        if turn:
            db.execute(
                'UPDATE turns SET status=? WHERE session=? '
                'AND (status="active" OR turn=?)',
                (status, sid, turn))
        else:
            db.execute('UPDATE turns SET status=? WHERE session=? AND status="active"',
                       (status, sid))

def receive(event, c, is_away=None):
    # 1. 确认实际进程来源；不依据会话标题、JSON 外形或子代理身份拦截。
    name = safe_text(event.get('hook_event_name', ''))
    sid = safe_text(event.get('session_id', ''))
    turn = safe_text(event.get('turn_id', ''))
    if name == 'UserPromptSubmit':
        if respire_background_source():
            finish_turn(sid, turn, 'background')
            return 'ignored_respire_background'
        return track_turn(event, c)
    if name == 'SessionEnd':
        finish_turn(sid, '', 'ended')
        return 'closed'
    if name not in ('Stop', 'Interrupt') or not sid or not turn:
        return 'ignored'
    finish_turn(sid, turn, 'completed' if name == 'Stop' else 'interrupted')
    # 2. 只静默已确认后台的保存回执和分块提炼；中断、失败及未知输出继续按原策略通知。
    respire_source = name == 'Stop' and respire_background_source()
    output_kind = respire_output_kind(event.get('last_assistant_message')) if respire_source else 'not_checked'
    respire_success = respire_source and output_kind in ('receipt', 'extraction')
    if name == 'Stop':
        # 仅记录判定枚举，后续可区分来源未命中和协议未命中，不保存正文、来源片段或记录 IDs。
        diagnostic('background_filter', session=sid, respire_source=respire_source,
                   output_kind=output_kind)
    # Repeated Stop continuation and approval retries must not create duplicate mail.
    eid = hashlib.sha256(f'{sid}|{turn}|{name}'.encode()).hexdigest()
    status = 'suppressed'
    if respire_success:
        status = 'suppressed_respire'
    elif session_enabled(sid, c) and c['mode'] == 'always':
        status = 'pending'
    elif session_enabled(sid, c) and c['mode'] == 'auto':
        # Decide at event time. A reply completed while the user is present must
        # never remain queued and become mail merely because the PC is locked later.
        away_now = away(c['idle_seconds']) if is_away is None else is_away
        status = 'pending' if away_now else 'suppressed'
    if not respire_success and quiet_hours(c):
        status = 'suppressed'
    # 3. 只为待发事件读取正文；被过滤事件仅保存可去重的元数据。
    message = ''
    if status == 'pending' and name == 'Stop' and c.get('include_message', True):
        message = latest_assistant_message(sid, event)
    if status == 'pending' and c.get('include_question', True):
        question = turn_question(sid, event)
        message = (f'本轮提问：\n{question or "未取得对应轮次提问"}\n\n回答：\n{message}'
                   if message else f'本轮提问：\n{question or "未取得对应轮次提问"}')
    with database() as db:
        cur = db.execute('INSERT OR IGNORE INTO events(id, created, session, turn, event, status, message) VALUES(?,?,?,?,?,?,?)',
                         (eid, time.time(), sid, turn, name, status, message))
        return status if cur.rowcount else 'duplicate'

def auto_action(created, c, is_away=None, now=None):
    away_now = away(c['idle_seconds']) if is_away is None else is_away
    if away_now:
        return 'send'
    if (time.time() if now is None else now) - created >= c['idle_seconds']:
        return 'suppress'
    return 'wait'

def password():
    # Export-Clixml protects the credential with Windows DPAPI for the current user.
    result = subprocess.run(['powershell.exe', '-NoProfile', '-NonInteractive', '-Command',
        "$c = Import-Clixml -LiteralPath '" + str(ROOT / 'smtp.credential.xml').replace("'", "''") + "'; [Console]::Write($c.GetNetworkCredential().Password)"],
        capture_output=True, text=True, check=True, creationflags=0x08000000)
    return result.stdout

def normalized_path(value):
    raw = str(value or '')
    if raw.startswith('\\\\?\\'):
        raw = raw[4:]
    return os.path.normcase(os.path.normpath(raw))

def clean_display(value, fallback):
    cleaned = ' '.join(str(value or '').split())
    return cleaned or fallback

def clean_message(value, limit=2000):
    cleaned = safe_text(value).replace('\r\n', '\n').replace('\r', '\n').strip()
    return cleaned if len(cleaned) <= limit else f'{cleaned[:limit - 3]}...'

def rollout_path(sid, event):
    candidate = safe_text(event.get('transcript_path', ''))
    if not candidate:
        state = codex_home() / 'state_5.sqlite'
        try:
            db = sqlite3.connect('file:' + state.as_posix() + '?mode=ro', uri=True, timeout=2)
            try:
                row = db.execute('SELECT rollout_path FROM threads WHERE id=?', (sid,)).fetchone()
                candidate = safe_text(row[0]) if row else ''
            finally:
                db.close()
        except (OSError, sqlite3.Error) as exc:
            diagnostic('transcript_lookup_failed', error=error_category(exc))
            return None
    try:
        path = Path(candidate).resolve()
        sessions = (codex_home() / 'sessions').resolve()
        path.relative_to(sessions)
        return path if path.is_file() else None
    except (OSError, ValueError) as exc:
        diagnostic('transcript_path_rejected', error=error_category(exc))
        return None

def turn_question(sid, event):
    """只读取指定轮次的 user_message，不从其他轮次或最新标题回填。"""
    path = rollout_path(sid, event)
    turn = safe_text(event.get('turn_id', ''))
    if not path or not turn:
        return ''
    active, questions = '', []
    try:
        with path.open(encoding='utf-8') as stream:
            for line in stream:
                try:
                    item = json.loads(line)
                except json.JSONDecodeError:
                    continue
                payload = item.get('payload', {})
                if item.get('type') == 'turn_context':
                    active = payload.get('turn_id', '')
                elif item.get('type') == 'event_msg':
                    if payload.get('type') == 'task_started':
                        active = payload.get('turn_id', '')
                    elif payload.get('type') == 'user_message' and payload.get('turn_id', active) == turn:
                        text = clean_message(payload.get('message', ''))
                        if text and text not in questions:
                            questions.append(text)
        return clean_message('\n'.join(questions), 2000)
    except OSError as exc:
        diagnostic('question_read_failed', error=error_category(exc))
        return ''

def latest_assistant_message(sid, event):
    direct = clean_message(event.get('last_assistant_message', ''))
    if direct:
        return direct
    path = rollout_path(sid, event)
    if not path:
        return ''
    try:
        with path.open('rb') as stream:
            stream.seek(0, os.SEEK_END)
            size = stream.tell()
            stream.seek(max(0, size - 2 * 1024 * 1024))
            tail = stream.read().decode('utf-8', errors='replace')
        for line in reversed(tail.splitlines()):
            try:
                item = json.loads(line)
            except json.JSONDecodeError:
                continue
            payload = item.get('payload', {})
            if (item.get('type') != 'response_item' or payload.get('type') != 'message'
                    or payload.get('role') != 'assistant'):
                continue
            parts = [safe_text(block.get('text', '')) for block in payload.get('content', [])
                     if block.get('type') in ('output_text', 'text')]
            return clean_message('\n'.join(part for part in parts if part))
    except OSError as exc:
        diagnostic('message_read_failed', error=error_category(exc))
    return ''

def thread_display(sid):
    """Resolve the stable session id to a human-readable project and title."""
    state = codex_home() / 'state_5.sqlite'
    project, title, cwd = '', '', ''
    try:
        db = sqlite3.connect('file:' + state.as_posix() + '?mode=ro', uri=True, timeout=2)
        try:
            row = db.execute('SELECT name,title,cwd,project_id FROM threads WHERE id=?', (sid,)).fetchone()
            if row:
                name, legacy_title, cwd, project_id = row
                title = name or legacy_title
                if project_id:
                    hit = db.execute('SELECT name FROM projects WHERE id=?', (project_id,)).fetchone()
                    project = hit[0] if hit else ''
                if not project:
                    current = normalized_path(cwd)
                    roots = db.execute('SELECT p.name,r.path FROM projects p JOIN project_roots r ON r.project_id=p.id').fetchall()
                    matches = [(name, normalized_path(root)) for name, root in roots
                               if current == normalized_path(root)
                               or current.startswith(normalized_path(root) + os.sep)]
                    if matches:
                        project = max(matches, key=lambda item: len(item[1]))[0]
        finally:
            db.close()
    except (OSError, sqlite3.Error) as exc:
        diagnostic('thread_lookup_failed', error=error_category(exc))
    if not project and cwd:
        project = Path(normalized_path(cwd)).name
    return clean_display(project, '未知项目'), clean_display(title, '未命名会话')

def build_message(row, c):
    label = {'Stop': '本轮处理结束', 'Interrupt': '本轮已中断',
             'Stalled': '会话疑似无进展'}.get(row[3], 'Codex 状态变化')
    project, title = thread_display(row[2])
    msg = EmailMessage()
    subject_title = title if len(title) <= 80 else f'{title[:77]}...'
    msg['Subject'] = f'Codex 通知：{project}｜{subject_title}｜{label}'
    msg['From'], msg['To'] = c['sender'], c['recipient']
    latest = clean_message(row[5] if len(row) > 5 else '', 4500)
    latest_block = f'\n最新内容：\n{latest}\n' if latest else ''
    detail = ('检测到该轮长时间没有新的转录活动，可能正在等待处理、工具阻塞或已经暂停。\n'
              if row[3] == 'Stalled' else '')
    msg.set_content(f'{label}\n项目：{project}\n会话：{title}\n时间：{time.strftime("%Y-%m-%d %H:%M:%S", time.localtime(row[1]))}\n{latest_block}\n{detail}请回到 Codex 查看结果或处理待办。\n')
    return msg

def send(row, c):
    if not all(c[k] for k in ('smtp_host', 'sender', 'recipient')):
        raise ValueError('SMTP settings incomplete')
    if c['smtp_security'] not in ('ssl', 'starttls'):
        raise ValueError('SMTP requires TLS')
    msg = build_message(row, c)
    ctx = ssl.create_default_context()
    cls = smtplib.SMTP_SSL if c['smtp_security'] == 'ssl' else smtplib.SMTP
    kwargs = {'context': ctx} if c['smtp_security'] == 'ssl' else {}
    with cls(c['smtp_host'], int(c['smtp_port']), timeout=20, **kwargs) as smtp:
        if c['smtp_security'] == 'starttls':
            smtp.starttls(context=ctx)
        smtp.login(c['sender'], password())
        smtp.send_message(msg)

def start_background(command):
    subprocess.Popen([sys.executable, __file__, command], creationflags=0x08000008,
                     stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL,
                     stderr=subprocess.DEVNULL)

def watchdog():
    """Notify once when an active turn has no transcript activity for the limit."""
    lock = open(ROOT / 'watchdog.lock', 'a+b')
    lock.seek(0)
    try:
        msvcrt.locking(lock.fileno(), msvcrt.LK_NBLCK, 1)
    except OSError:
        lock.close()
        return
    try:
        deadline = time.time() + WATCHDOG_MAX_SECONDS
        while time.time() < deadline:
            c = config()
            now = time.time()
            queued = False
            with database() as db:
                rows = db.execute(
                    'SELECT session,turn,started,last_activity,transcript,notified '
                    'FROM turns WHERE status="active"').fetchall()
                for sid, turn, started, last_activity, transcript, notified in rows:
                    if c['mode'] == 'off' or not session_enabled(sid, c):
                        db.execute('UPDATE turns SET status="disabled" WHERE session=? AND turn=?',
                                   (sid, turn))
                        continue
                    if transcript:
                        try:
                            mtime = Path(transcript).stat().st_mtime
                            if mtime > last_activity:
                                last_activity = mtime
                                db.execute(
                                    'UPDATE turns SET last_activity=? WHERE session=? AND turn=?',
                                    (mtime, sid, turn))
                        except OSError as exc:
                            diagnostic('watchdog_transcript_failed', error=error_category(exc))
                            db.execute(
                                'UPDATE turns SET transcript="" WHERE session=? AND turn=?',
                                (sid, turn))
                    threshold = max(60, int(c.get('stalled_seconds', DEFAULT['stalled_seconds'])))
                    if notified or now - last_activity < threshold:
                        continue
                    if quiet_hours(c, now):
                        db.execute('UPDATE turns SET status="quiet_suppressed",notified=1 WHERE session=? AND turn=?', (sid, turn))
                        continue
                    should_notify = c['mode'] == 'always'
                    if c['mode'] == 'auto':
                        should_notify = away(c['idle_seconds'])
                    if not should_notify:
                        continue
                    eid = hashlib.sha256(f'{sid}|{turn}|Stalled'.encode()).hexdigest()
                    db.execute(
                        'INSERT OR IGNORE INTO events(id,created,session,turn,event,status,message) '
                        'VALUES(?,?,?,?,"Stalled","pending","")',
                        (eid, now, sid, turn))
                    db.execute(
                        'UPDATE turns SET status="stalled",notified=1 WHERE session=? AND turn=?',
                        (sid, turn))
                    queued = True
                active = db.execute(
                    'SELECT COUNT(*) FROM turns WHERE status="active"').fetchone()[0]
            if queued:
                start_background('worker')
            if not active:
                return
            time.sleep(WATCHDOG_POLL_SECONDS)
    except Exception as exc:
        diagnostic('watchdog_failed', error=error_category(exc))
    finally:
        lock.close()

def worker():
    lock = open(ROOT / 'worker.lock', 'a+b')
    lock.seek(0)
    try:
        msvcrt.locking(lock.fileno(), msvcrt.LK_NBLCK, 1)
    except OSError:
        lock.close()
        return
    try:
        deadline = time.time() + max(150, int(config()['idle_seconds']) + 60)
        while time.time() < deadline:
            c = config()
            with database() as db:
                rows = db.execute('SELECT id,created,session,event,attempts,message,turn FROM events WHERE status="pending" AND next_try<=?', (time.time(),)).fetchall()
                for row in rows:
                    if c['mode'] == 'off' or not session_enabled(row[2], c) or quiet_hours(c) or quiet_hours(c, row[1]):
                        db.execute('UPDATE events SET status="suppressed",message="" WHERE id=?', (row[0],))
                        db.commit()
                        continue
                    if c['mode'] == 'auto':
                        action = auto_action(row[1], c)
                        if action == 'wait':
                            continue
                        if action == 'suppress':
                            db.execute('UPDATE events SET status="suppressed",message="" WHERE id=?', (row[0],))
                            db.commit()
                            continue
                    try:
                        send(row, c)
                        db.execute('UPDATE events SET status="sent",error="",message="" WHERE id=?', (row[0],))
                    except Exception as exc:
                        n = row[4] + 1
                        state = 'failed' if n >= 3 else 'pending'
                        category = error_category(exc)
                        # Keep logs free of credentials and message bodies.
                        db.execute('UPDATE events SET attempts=?,status=?,next_try=?,error=?,message=CASE WHEN ?="failed" THEN "" ELSE message END WHERE id=?',
                            (n, state, time.time()+30, category, state, row[0]))
                        diagnostic('send_failed', error=category, attempt=n,
                                   final=state == 'failed', event=row[3])
                    db.commit()
                pending = db.execute('SELECT COUNT(*) FROM events WHERE status="pending"').fetchone()[0]
            if not pending:
                return
            time.sleep(2)
    finally:
        lock.close()

def gui():
    import tkinter as tk
    from tkinter import ttk, messagebox
    c = config()
    app = tk.Tk()
    app.title('Codex 邮件通知设置')
    app.geometry('720x820')
    variables = {}
    fields = [('mode', '通知模式', ['off', 'always', 'auto']),
              ('idle_seconds', '空闲秒数（自动模式）', None),
              ('stalled_seconds', '疑似无进展秒数', None),
              ('quiet_start', '免打扰开始（HH:MM）', None),
              ('quiet_end', '免打扰结束（HH:MM）', None),
              ('retention_days', '事件记录保留天数', None),
              ('smtp_host', 'SMTP 服务器', None), ('smtp_port', 'SMTP 端口', None),
              ('smtp_security', '加密方式', ['ssl', 'starttls']),
              ('sender', '发送邮箱', None), ('recipient', '接收邮箱', None)]
    for i, (key, label, choices) in enumerate(fields):
        ttk.Label(app, text=label).grid(row=i, column=0, padx=16, pady=8, sticky='w')
        v = tk.StringVar(value=str(c[key])); variables[key] = v
        widget = ttk.Combobox(app, textvariable=v, values=choices, state='readonly') if choices else ttk.Entry(app, textvariable=v, width=48)
        widget.grid(row=i, column=1, padx=10, sticky='ew')
    include_message = tk.BooleanVar(value=bool(c.get('include_message', True)))
    quiet_enabled = tk.BooleanVar(value=c.get('quiet_enabled', True))
    include_question = tk.BooleanVar(value=c.get('include_question', True))
    ttk.Checkbutton(app, text='启用免打扰（本地时间；优先于始终通知；不补发）', variable=quiet_enabled).grid(row=len(fields), columnspan=2, pady=6)
    ttk.Checkbutton(app, text='邮件包含本轮提问（发送后从队列清除）', variable=include_question).grid(row=len(fields)+1, columnspan=2, pady=6)
    body_row = len(fields)+2
    ttk.Checkbutton(app, text='邮件包含最新回复正文（发送后立即从本地队列清除）',
                    variable=include_message).grid(row=body_row, columnspan=2, pady=8)
    ttk.Label(app, text='off=关闭；always=始终通知；auto=锁屏或离席时通知').grid(row=body_row+1, columnspan=2, pady=4)
    ttk.Label(app, text='无进展检测依据转录文件活动，只提示“疑似”，不会监听批准请求。').grid(row=body_row+2, columnspan=2, pady=4)
    ttk.Label(app, text='启用通知的会话 ID（留空=全部；填写后仅指定会话，每行一个）').grid(row=body_row+3, columnspan=2)
    sessions = tk.Text(app, height=5, width=68)
    sessions.grid(row=body_row+4, columnspan=2, padx=16)
    sessions.insert('1.0', '\n'.join(c['sessions']))
    def apply():
        try:
            updated = c | {k: v.get().strip() for k,v in variables.items()}
            updated['idle_seconds'] = int(updated['idle_seconds'])
            updated['stalled_seconds'] = int(updated['stalled_seconds'])
            updated['retention_days'] = int(updated['retention_days'])
            updated['smtp_port'] = int(updated['smtp_port'])
            updated['include_message'] = bool(include_message.get())
            updated['include_question'] = bool(include_question.get())
            updated['quiet_enabled'] = bool(quiet_enabled.get())
            if clock_minutes(updated['quiet_start']) == clock_minutes(updated['quiet_end']):
                raise ValueError('免打扰开始与结束时间不能相同')
            if (updated['idle_seconds'] < 1
                    or not 60 <= updated['stalled_seconds'] <= WATCHDOG_MAX_SECONDS
                    or not 1 <= updated['retention_days'] <= 3650
                    or not 1 <= updated['smtp_port'] <= 65535):
                raise ValueError('空闲时间、无进展阈值、保留天数或端口无效')
            updated['sessions'] = list(dict.fromkeys(s.strip() for s in sessions.get('1.0','end').splitlines() if s.strip()))
            save(updated)
            messagebox.showinfo('已保存', '设置即时生效。SMTP 授权码请用保存授权码脚本配置。')
        except ValueError as exc:
            messagebox.showerror('设置错误', str(exc))
    ttk.Button(app, text='保存设置', command=apply).grid(row=body_row+5,columnspan=2,pady=14)
    app.mainloop()

if __name__ == '__main__':
    for stream in (sys.stdin, sys.stdout, sys.stderr):
        if hasattr(stream, 'reconfigure'):
            stream.reconfigure(encoding='utf-8', errors='replace')
    command = sys.argv[1] if len(sys.argv)>1 else 'settings'
    if command == 'hook':
        try:
            parser = argparse.ArgumentParser(prog='notifier.py hook')
            parser.add_argument('--codex-home')
            options = parser.parse_args(sys.argv[2:])
            if options.codex_home:
                # 安装器把实际目录固定在 Hook 命令中，后台子进程继续继承这个值。
                os.environ['CODEX_HOME'] = str(Path(options.codex_home).expanduser().resolve())
            diagnostic('invoked', user=os.environ.get('USERNAME', 'unknown'))
            event = json.load(sys.stdin)
            state = receive(event, config())
            diagnostic('received', event=safe_text(event.get('hook_event_name', '')),
                       session=safe_text(event.get('session_id', '')),
                       has_turn=bool(event.get('turn_id')),
                       has_message=bool(event.get('last_assistant_message')),
                       has_transcript=bool(event.get('transcript_path')),
                       state=state)
            if state == 'pending':
                start_background('worker')
            elif state == 'watching':
                start_background('watchdog')
        except Exception as exc:
            try:
                diagnostic('error', error=error_category(exc))
            except Exception:
                pass
            print('Notifier: '+error_category(exc), file=sys.stderr)
        # Never approve, reject, or request a continuation of Codex work.
        print('{}')
    elif command == 'worker':
        worker()
    elif command == 'watchdog':
        watchdog()
    elif command == 'settings':
        gui()
    elif command == 'status':
        with database() as db:
            c = config()
            print(json.dumps({
                'mode': c['mode'],
                'smtp_configured': bool(c['smtp_host']),
                'credential_saved': (ROOT/'smtp.credential.xml').exists(),
                'include_message': bool(c.get('include_message', True)),
                'retention_days': int(c.get('retention_days', DEFAULT['retention_days'])),
                'stalled_seconds': int(c.get('stalled_seconds', DEFAULT['stalled_seconds'])),
                'events': db.execute('SELECT status,COUNT(*) FROM events GROUP BY status').fetchall(),
                'errors': db.execute('SELECT error,COUNT(*) FROM events WHERE error!="" GROUP BY error').fetchall(),
                'active_turns': db.execute('SELECT COUNT(*) FROM turns WHERE status="active"').fetchone()[0]
            }, ensure_ascii=False))
