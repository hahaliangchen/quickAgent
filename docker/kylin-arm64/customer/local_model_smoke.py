"""Verify a customer-owned keyless provider against a local disposable endpoint."""
import json
from pathlib import Path
import time
import urllib.error
import urllib.request

HERE = Path(__file__).resolve().parent


def verify(run, dc, base, authorization, renderer_id):
    run(['docker', 'cp', str(HERE / 'local-model-smoke.cjs'), renderer_id + ':/tmp/local-model-smoke.cjs'])
    dc('exec', '-d', 'renderer', 'node', '/tmp/local-model-smoke.cjs')
    dc('exec', '-T', 'app', 'python', '-c',
       "import time,urllib.request\n"
       "for attempt in range(60):\n"
       " try:\n"
       "  urllib.request.urlopen('http://renderer:9099/v1/models',timeout=1).close();break\n"
       " except OSError:\n"
       "  if attempt==59:raise\n"
       "  time.sleep(.25)\n")
    model_id = 'customer-qwen3-vl-32b'
    provider_id = 'qwen-local'
    model_url = 'http://renderer:9099/v1'

    def request(path, body=None, method=None):
        headers = {'Authorization': authorization}
        if body is not None:
            headers['Content-Type'] = 'application/json'
        req = urllib.request.Request(base + path, data=None if body is None else json.dumps(body).encode(),
                                     headers=headers, method=method)
        try:
            with urllib.request.urlopen(req, timeout=90) as response:
                return json.load(response)
        except urllib.error.HTTPError as error:
            raise AssertionError(f'{path}: HTTP {error.code}: {error.read().decode()[:2000]}') from error

    # The search selection is deliberately left on a DeepSeek provider with no
    # key: saving and using this independent local model must still succeed.
    saved = request('/agent/settings/data')
    assert not any(p['has_api_key'] for p in saved['providers'])
    providers = saved['providers'] + [{'id': provider_id, 'display_name': 'Customer Qwen', 'url': model_url,
                                     'api_type': 'openai-completions', 'models': [model_id]}]
    payload = {'revision': saved['revision'], 'providers': providers, 'web_search_provider': 'deepseek',
               'orchestrator_provider': provider_id, 'orchestrator_model': model_id,
               'visual_tool': {}, 'delegate_agent': {}, 'reasoning_effort': '', 'fast_mode': False}
    saved = request('/agent/settings/data', payload, 'PUT')
    local = next(p for p in saved['providers'] if p['id'] == provider_id)
    assert not local['has_api_key'] and not local['api_key_env']
    discovered = request('/agent/settings/discover', {'id': provider_id, 'url': model_url})
    assert model_id in discovered['models']
    catalog = request('/agent/models')
    assert any(m['provider'] == provider_id and m['id'] == model_id and m['enabled'] for m in catalog['models'])
    created = request('/agent/tasks', {'prompt': 'Say hello. Do not use tools.', 'model': model_id, 'max_steps': 1})
    deadline = time.monotonic() + 120
    while time.monotonic() < deadline:
        task = request('/agent/tasks/' + created['task_id'])
        if task['status'] not in {'running', 'cancelling'}:
            break
        time.sleep(.5)
    assert task['status'] == 'completed', task
    calls = json.loads(dc('exec', '-T', 'app', 'python', '-c',
                          "import json,urllib.request;print(json.dumps(json.load(urllib.request.urlopen('http://renderer:9099/requests'))))", capture=True))
    assert calls and all(call['model'] == model_id and not call['authorization'] for call in calls)
    dc('restart', 'app')
    dc('up', '-d', '--wait', '--wait-timeout', '480', 'app')
    restarted = request('/agent/settings/data')
    assert restarted['orchestrator_provider'] == provider_id
    assert restarted['orchestrator_model'] == model_id
    local = next(p for p in restarted['providers'] if p['id'] == provider_id)
    assert not local['has_api_key'] and local['url'] == model_url
    return {'keyless_model_saved': True, 'keyless_model_called': True, 'keyless_model_restart': True,
            'model_discovery_without_key': True, 'missing_search_key_does_not_block_models': True,
            'keyless_model_execution': 'disposable OpenAI-compatible fixture'}
