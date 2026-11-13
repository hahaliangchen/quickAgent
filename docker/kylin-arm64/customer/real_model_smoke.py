"""A short real Qwen call using only the disposable customer's saved settings."""
import json
import time
import urllib.error
import urllib.request


def verify(base, authorization, model_id, fallback_url):
    def request(path, body=None, method=None):
        headers = {'Authorization': authorization}
        if body is not None:
            headers['Content-Type'] = 'application/json'
        req = urllib.request.Request(base + path, data=None if body is None else json.dumps(body).encode(),
                                     headers=headers, method=method)
        with urllib.request.urlopen(req, timeout=60) as response:
            return json.load(response)

    saved = request('/agent/settings/data')
    assert len(saved['providers']) == 1 and not saved['providers'][0]['has_api_key']
    attempts = []
    for index in range(2 if fallback_url else 1):
        if index:
            latest = request('/agent/settings/data')
            providers = latest['providers']
            providers[0]['url'] = fallback_url
            saved = request('/agent/settings/data', {
                'revision': latest['revision'], 'providers': providers, 'web_search_provider': '',
                'orchestrator_provider': 'qwen-test', 'orchestrator_model': model_id,
                'reasoning_effort': '', 'fast_mode': False, 'visual_tool': {}, 'delegate_agent': {},
            }, 'PUT')
        configured_url = saved['providers'][0]['url']
        created = request('/agent/tasks', {'prompt': '只回复 OK。不要调用工具，不要执行程序。', 'model': model_id, 'max_steps': 1})
        task_id = created['task_id']
        deadline = time.monotonic() + 180
        while time.monotonic() < deadline:
            task = request('/agent/tasks/' + task_id)
            if task['status'] not in {'running', 'cancelling'}:
                break
            time.sleep(.5)
        events = request('/agent/tasks/' + task_id + '/events')['events']
        # Read actual user-facing deltas/messages, never infer success from HTTP alone.
        messages = [event.get('data', {}).get('message', {}).get('content', '')
                    for event in events if event.get('type') == 'assistant/message']
        answer = ''.join(part for part in messages if isinstance(part, str))
        if not answer:
            answer = ''.join(str(event.get('data', {}).get('text', '')) for event in events if event.get('type') == 'assistant/delta')
        errors = [event.get('data') for event in events if 'error' in event.get('type', '')]
        attempts.append({'url': configured_url, 'status': task['status'], 'answer': answer[:300], 'errors': errors})
        if task['status'] == 'completed' and 'OK' in answer.upper():
            return {'real_qwen_configured': True, 'real_qwen_called': True, 'qwen_attempts': attempts,
                    'real_qwen_model': model_id, 'real_qwen_url': configured_url}
        if task['status'] in {'running', 'cancelling'}:
            request('/agent/tasks/' + task_id, method='DELETE')
    raise AssertionError('Real Qwen call did not complete: ' + json.dumps(attempts, ensure_ascii=False))
