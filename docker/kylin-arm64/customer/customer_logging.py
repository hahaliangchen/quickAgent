"""Persistent customer diagnostics; no prompts, document bodies or credentials export."""
import contextlib
from datetime import datetime, timezone
import json
import logging
from logging.handlers import RotatingFileHandler
import os
from pathlib import Path
import re
import sqlite3
import threading
from urllib.parse import quote, urlsplit, urlunsplit

EVENTS = ('program/source_invalid', 'program/error', 'program/start', 'turn/end',
          'tool/result', 'tool/async_result', 'subagent/event')
SECRET_NAME = re.compile(r'password|api.?key|authorization|token|jwt|secret|signing.?key|cookie', re.I)
ANSI = re.compile(r'\x1b\[[0-9;]*[A-Za-z]')


def redact(value, secrets=()):
    if isinstance(value, dict):
        return {k: '[REDACTED]' if SECRET_NAME.search(k) else redact(v, secrets) for k, v in value.items()}
    if isinstance(value, list):
        return [redact(v, secrets) for v in value]
    if not isinstance(value, str):
        return value
    text = ANSI.sub('', value)
    for secret in sorted(set(secrets), key=len, reverse=True):
        if len(secret) >= 4:
            text = text.replace(secret, '[REDACTED]')
    text = re.sub(r'(?i)\b(Bearer|Basic)\s+[A-Za-z0-9._~+/=-]+', r'\1 [REDACTED]', text)
    text = re.sub(r'\bsk-[A-Za-z0-9_-]{8,}', '[REDACTED]', text)
    text = re.sub(r'(?i)([\w-]*(?:password|api[_-]?key|(?:access[_-]?)?token|jwt|authorization|cookie|secret|signing[_-]?key)[\w-]*["\']?\s*[:=]\s*)("[^"\n]*"|\'[^\'\n]*\'|[^\s,;}]+)', r'\1[REDACTED]', text)
    text = re.sub(r'https?://[^\s"<>]+', lambda match: safe_url(match.group()), text)
    return text[:12000]


def safe_url(value):
    try:
        u = urlsplit(value)
        host = u.hostname or ''
        if ':' in host:
            host = '[' + host + ']'
        if u.port:
            host += ':' + str(u.port)
        return urlunsplit((u.scheme, host, u.path, '', ''))
    except ValueError:
        return '[invalid URL]'


def diagnostic(value):
    if not isinstance(value, dict):
        return {'message': str(value)[:4000]}
    # Source excerpts and tool arguments may contain customer documents.
    fields = ('code', 'error', 'message', 'phase', 'operation', 'executed', 'retryable',
              'line', 'column', 'status', 'error_code', 'host_denied')
    result = {key: value[key] for key in fields if key in value}
    if 'diagnostics' in value:
        result['diagnostics'] = [diagnostic(item) for item in value['diagnostics'][:12]] if isinstance(value['diagnostics'], list) else diagnostic(value['diagnostics'])
    if isinstance(value.get('error'), dict):
        result['error'] = diagnostic(value['error'])
    return result


def event_detail(kind, data):
    if kind == 'subagent/event':
        return event_detail(data.get('event_type', ''), data.get('event', {}))
    if kind in ('program/source_invalid', 'program/error'):
        return diagnostic(data.get('diagnostic', data.get('error', data)))
    if kind == 'program/start':
        return {'status': 'installed', 'resource': data.get('source_resource'), 'turn': data.get('turn')}
    if kind == 'turn/end':
        reason = data.get('reason', {})
        return {'turn': data.get('turn'), 'status': reason.get('kind'), 'error': diagnostic(reason['error'])} if reason.get('error') else {'turn': data.get('turn'), 'status': reason.get('kind')}
    if kind in ('tool/result', 'tool/async_result'):
        result = data.get('meta', {}).get('result', {})
        if not isinstance(result, dict):
            return None
        if result.get('error') or result.get('error_code') or result.get('status') == 'failed' or result.get('host_denied'):
            return diagnostic(result)
    return None


class PrivateRotatingHandler(RotatingFileHandler):
    def _open(self):
        stream = super()._open()
        Path(self.baseFilename).chmod(0o600)
        return stream


class CustomerLog:
    def __init__(self, directory, *, max_bytes=5 * 1024 * 1024, backups=4, secrets=None):
        self.directory = Path(directory)
        self.directory.mkdir(parents=True, exist_ok=True)
        self.secrets = list(secrets) if secrets is not None else [v for k, v in os.environ.items() if SECRET_NAME.search(k)]
        self.handler = PrivateRotatingHandler(self.directory / 'customer.log', maxBytes=max_bytes, backupCount=backups, encoding='utf-8')
        self.handler.setFormatter(logging.Formatter('%(message)s'))
        self.logger = logging.Logger('customer-diagnostics-' + str(id(self)), level=logging.INFO)
        self.logger.addHandler(self.handler)
        (self.directory / 'customer.log').chmod(0o600)

    def write(self, service, event, **fields):
        record = {'time': datetime.now(timezone.utc).isoformat(), 'service': service, 'event': event, **fields}
        self.logger.info(json.dumps(redact(record, self.secrets), ensure_ascii=False))

    def close(self):
        self.handler.close()


class EventCollector:
    def __init__(self, state, log):
        self.state, self.log = Path(state), log
        self.checkpoint = log.directory / '.event-cursor.json'
        try:
            saved = json.loads(self.checkpoint.read_text())
        except (OSError, ValueError):
            saved = {}
        self.since = saved.get('since', 0)
        self.cursors = saved.get('tasks', {})
        self.last_error = None

    def poll(self):
        db = self.state / 'workspace/.codex-workspace-mcp/codex_state.db'
        if not db.is_file():
            return
        changed = False
        with contextlib.closing(sqlite3.connect('file:' + quote(str(db)) + '?mode=ro', uri=True, timeout=1)) as conn:
            conn.execute('PRAGMA query_only=ON')
            tasks = conn.execute('SELECT id, updated_at, model, provider_name FROM agent_tasks WHERE updated_at>=? ORDER BY updated_at,id', (self.since,)).fetchall()
            for task, updated, model, provider in tasks:
                previous = self.cursors.get(task, -1)
                high = conn.execute('SELECT MAX(seq) FROM agent_task_events WHERE task_id=?', (task,)).fetchone()[0]
                if high is None:
                    continue
                if high < previous:
                    previous = -1
                if high != previous:
                    marks = ','.join('?' for _ in EVENTS)
                    rows = conn.execute(f'SELECT seq,timestamp,kind,data FROM agent_task_events WHERE task_id=? AND seq>? AND seq<=? AND kind IN ({marks}) ORDER BY seq', (task, previous, high, *EVENTS)).fetchall()
                    for seq, timestamp, kind, raw in rows:
                        detail = event_detail(kind, json.loads(raw))
                        if detail is not None:
                            self.log.write('agent', kind, task_id=task, seq=seq, event_timestamp=timestamp, provider=provider, model=model, detail=detail)
                    self.cursors[task] = high
                    changed = True
                self.since = max(self.since, updated)
        if changed:
            temporary = self.checkpoint.with_suffix('.tmp')
            temporary.write_text(json.dumps({'since': self.since, 'tasks': self.cursors}))
            temporary.chmod(0o600)
            temporary.replace(self.checkpoint)

    def run(self, stop):
        while not stop.is_set():
            try:
                self.poll()
                self.last_error = None
            except (sqlite3.Error, OSError, ValueError) as error:
                message = str(error)
                if message != self.last_error:
                    self.log.write('diagnostics', 'collector_error', message=message)
                    self.last_error = message
            stop.wait(1)
        try:
            self.poll()
        except (sqlite3.Error, OSError, ValueError) as error:
            self.log.write('diagnostics', 'collector_error', message=str(error))


def stream_output(pipe, service, log):
    try:
        while True:
            line = pipe.readline(16384)
            if not line:
                break
            text = line.decode('utf-8', errors='replace').rstrip()
            log.write(service, 'runtime', message=text)
            # Preserve docker compose logs, applying the same credential scrub.
            print('[' + service + '] ' + redact(text, log.secrets), flush=True)
    finally:
        pipe.close()


def start_collector(state, log):
    stop = threading.Event()
    collector = EventCollector(state, log)
    thread = threading.Thread(target=collector.run, args=(stop,), daemon=True)
    thread.start()
    return stop, thread
