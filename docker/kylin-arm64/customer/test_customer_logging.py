import io
import json
from pathlib import Path
import sqlite3
import tarfile
import tempfile
import unittest

from customer_logging import CustomerLog, EventCollector, redact, stream_output
from diagnostics import collect


class CustomerLoggingTests(unittest.TestCase):
    def fixture(self, root):
        state = root / 'state'
        db = state / 'workspace/.codex-workspace-mcp/codex_state.db'
        db.parent.mkdir(parents=True)
        conn = sqlite3.connect(db)
        conn.executescript('CREATE TABLE agent_tasks(id TEXT PRIMARY KEY,updated_at INTEGER,model TEXT,provider_name TEXT,status TEXT); CREATE TABLE agent_task_events(task_id TEXT,seq INTEGER,timestamp INTEGER,kind TEXT,data TEXT,PRIMARY KEY(task_id,seq));')
        conn.execute("INSERT INTO agent_tasks VALUES ('task-1',10,'qwen3-vl-32b','local','failed')")
        conn.commit()
        return state, conn

    def insert(self, conn, seq, kind, data):
        conn.execute('INSERT INTO agent_task_events VALUES (?,?,?,?,?)', ('task-1',seq,10,kind,json.dumps(data)))
        conn.commit()

    def test_specific_program_diagnostic_is_persisted_without_source_or_prompt(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            state, conn = self.fixture(root)
            self.insert(conn,0,'user/message',{'message':{'content':'private customer document'}})
            self.insert(conn,1,'program/source_invalid',{'diagnostic':{'code':'PROGRAM_SOURCE_INVALID','error':'provided module parameters do not match its declaration','diagnostics':[{'message':'invalid binding','line':42,'source_excerpt':'private source'}]}})
            log = CustomerLog(root/'logs')
            EventCollector(state,log).poll()
            log.close(); conn.close()
            raw = (root/'logs/customer.log').read_text()
            row = json.loads(raw)
            self.assertEqual(row['task_id'],'task-1')
            self.assertEqual(row['model'],'qwen3-vl-32b')
            self.assertEqual(row['detail']['diagnostics'][0]['line'],42)
            self.assertIn('provided module parameters',raw)
            self.assertNotIn('private source',raw)
            self.assertNotIn('private customer document',raw)

    def test_cursor_survives_restart_and_same_timestamp_late_error_is_not_lost(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp);state,conn = self.fixture(root)
            self.insert(conn,1,'turn/end',{'reason':{'kind':'error','error':{'message':'HTTP 401'}}})
            log=CustomerLog(root/'logs');collector=EventCollector(state,log)
            collector.poll();collector.poll()
            collector=EventCollector(state,log);collector.poll()
            self.insert(conn,2,'program/error',{'error':{'message':'MODULE_NOT_FOUND'}})
            collector.poll();log.close();conn.close()
            rows=[json.loads(line) for line in (root/'logs/customer.log').read_text().splitlines()]
            self.assertEqual([r['seq'] for r in rows],[1,2])

    def test_rotation_is_bounded(self):
        with tempfile.TemporaryDirectory() as temp:
            log=CustomerLog(temp,max_bytes=600,backups=2)
            for i in range(30):log.write('agent','test',message='x'*100,index=i)
            log.close()
            files=list(Path(temp).glob('customer.log*'))
            self.assertEqual(len(files),3)
            self.assertLess(sum(p.stat().st_size for p in files),1800)
            self.assertTrue(all(p.stat().st_mode & 0o777 == 0o600 for p in files))

    def test_secrets_and_url_credentials_are_redacted(self):
        value={'api_key':'private','message':'Bearer topsecret MYSQL_PASSWORD=abc123 https://user:pass@model:8000/v1?key=bad sk-abcdefgh123456 explicit-secret','diagnostic':{'line':12}}
        text=json.dumps(redact(value,['explicit-secret']))
        for secret in ['topsecret','abc123','user:pass','key=bad','sk-abcdefgh123456','explicit-secret','private']:
            self.assertNotIn(secret,text)
        self.assertIn('http',text);self.assertIn('model:8000/v1',text)
        self.assertNotIn('opaque-jwt',redact('token=opaque-jwt jwt=opaque-jwt Cookie=opaque-jwt'))

    def test_export_recovers_old_release_errors_without_exporting_credentials_or_database(self):
        with tempfile.TemporaryDirectory() as temp:
            root=Path(temp);state,conn=self.fixture(root)
            self.insert(conn,1,'program/source_invalid',{'diagnostic':{'code':'PROGRAM_SOURCE_INVALID','error':'PLUGIN_PROGRAM_HASH_MISMATCH'}})
            conn.close()
            profile=state/'agent-config';profile.mkdir()
            (profile/'providers.yml').write_text('[{"id":"llm-pi-ai","config":{"providers":{"local":{"baseURL":"http://user:password@model:8000/v1?key=secret","api":"openai-completions","apiKey":"private-model-key","models":[{"id":"qwen"}]}}}}]')
            (profile/'credentials.yaml').write_text('private-credentials')
            (state/'.env').write_text('private-environment')
            (state/'project.pptx').write_text('private-deck')
            logs=root/'logs';log=CustomerLog(logs);log.write('agent','runtime',message='Bearer sensitive-token');log.close()
            out=collect(state,logs,logs/'support.tar.gz')
            with tarfile.open(out) as archive:
                names=archive.getnames()
                text='\n'.join(archive.extractfile(name).read().decode() for name in names)
            self.assertIn('PLUGIN_PROGRAM_HASH_MISMATCH',text)
            for forbidden in ['private-model-key','private-credentials','private-environment','private-deck','sensitive-token','user:password','key=secret']:
                self.assertNotIn(forbidden,text)
            self.assertTrue(all(name.startswith(('events/','logs/')) or name=='summary.json' for name in names))

    def test_child_output_is_captured_and_scrubbed(self):
        with tempfile.TemporaryDirectory() as temp:
            log=CustomerLog(temp,secrets=['fake-customer-password'])
            stream_output(io.BytesIO(b'connection failed\npassword=fake-customer-password\n'),'backend',log)
            log.close()
            text=(Path(temp)/'customer.log').read_text()
            self.assertIn('connection failed',text)
            self.assertNotIn('fake-customer-password',text)


if __name__=='__main__':unittest.main()
