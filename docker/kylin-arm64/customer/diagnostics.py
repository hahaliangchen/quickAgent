#!/usr/bin/env python3
"""Export bounded support evidence, including diagnostics from existing releases."""
import argparse
from datetime import datetime, timezone
import io
import json
from pathlib import Path
import sqlite3
import tarfile
import tempfile
import contextlib
from urllib.parse import quote

from customer_logging import CustomerLog, EventCollector, redact, safe_url

MAX_FILE = 5 * 1024 * 1024
MAX_ARCHIVE = 30 * 1024 * 1024


def collect(state, logs, output):
    state, logs, output = Path(state), Path(logs), Path(output)
    output.parent.mkdir(parents=True, exist_ok=True)
    entries = {}
    notes = []
    def add(name, data):
        if isinstance(data, str):
            data = data.encode('utf-8')
        entries[name] = data
    # Re-extract durable errors even when the customer has not yet installed
    # continuous logging. SQLite is opened read-only, with normal WAL semantics.
    with tempfile.TemporaryDirectory() as temp:
        log = CustomerLog(temp)
        try:
            EventCollector(state, log).poll()
        except (OSError, sqlite3.Error, ValueError) as error:
            notes.append('Event collection: ' + str(error))
        finally:
            log.close()
        for path in sorted(Path(temp).glob('customer.log*')):
            add('events/' + path.name, path.read_bytes())
    # Only explicit log files are included. Never archive state/, .env, model
    # credentials, a whole database, customer documents or generated decks.
    remaining = MAX_ARCHIVE
    candidates = [(path, 'logs/' + path.name) for path in sorted(logs.glob('customer.log*'))]
    candidates += [(path, 'transport/' + path.name) for path in sorted((state/'workspace/logs').glob('model-requests-*.jsonl'))[-3:]]
    for path, name in candidates:
        if not path.is_file() or path.is_symlink() or remaining <= 0:
            continue
        with path.open('rb') as stream:
            size = path.stat().st_size
            take = min(size, MAX_FILE, remaining)
            stream.seek(max(0, size - take))
            if size > take:
                stream.readline()
            data = stream.read(take).decode('utf-8', errors='replace')
        data = redact(data).encode()
        add(name, data)
        remaining -= len(data)
    profile = state / 'agent-config/providers.yml'
    providers = []
    try:
        import yaml
        for patch in yaml.safe_load(profile.read_text()) or []:
            for identifier, config in patch.get('config', {}).get('providers', {}).items():
                providers.append({'id': identifier, 'url': safe_url(config.get('baseURL', '')),
                                  'api': config.get('api'), 'models': [m.get('id') if isinstance(m, dict) else m for m in config.get('models', [])]})
    except (OSError, ValueError, TypeError) as error:
        notes.append('Model route collection: ' + str(error))
    db = state / 'workspace/.codex-workspace-mcp/codex_state.db'
    tasks = []
    if db.is_file():
        try:
            with contextlib.closing(sqlite3.connect('file:' + quote(str(db)) + '?mode=ro', uri=True, timeout=2)) as conn:
                tasks = [{'task_id': r[0], 'status': r[1], 'provider': r[2], 'model': r[3], 'updated_at': r[4]} for r in conn.execute('SELECT id,status,provider_name,model,updated_at FROM agent_tasks ORDER BY updated_at DESC LIMIT 100')]
        except sqlite3.Error as error:
            notes.append('Task status collection: ' + str(error))
    add('summary.json', json.dumps(redact({'collected_at': datetime.now(timezone.utc).isoformat(), 'providers': providers, 'recent_tasks': tasks, 'notes': notes,
        'excluded': ['credentials', '.env', 'database', 'prompts', 'documents', 'generated presentations']}), ensure_ascii=False, indent=2) + '\n')
    temporary = output.with_name(output.name + '.tmp')
    try:
        with tarfile.open(temporary, 'w:gz') as archive:
            for name, data in entries.items():
                info = tarfile.TarInfo(name)
                info.size, info.mode = len(data), 0o600
                archive.addfile(info, io.BytesIO(data))
        temporary.chmod(0o600)
        temporary.replace(output)
    finally:
        temporary.unlink(missing_ok=True)
    return output


if __name__ == '__main__':
    parser = argparse.ArgumentParser()
    parser.add_argument('--state', type=Path, default=Path('/state'))
    parser.add_argument('--logs', type=Path, default=Path('/logs'))
    parser.add_argument('--output', type=Path, required=True)
    args = parser.parse_args()
    print(collect(args.state, args.logs, args.output))
