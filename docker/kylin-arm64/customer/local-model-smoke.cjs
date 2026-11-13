// Disposable OpenAI-compatible endpoint for the isolated customer smoke project.
const http = require('node:http');
const model = 'customer-qwen3-vl-32b';
const calls = [];
http.createServer(async (request, response) => {
    if ('authorization' in request.headers) {
        response.writeHead(400, {'Content-Type':'application/json'});
        return response.end(JSON.stringify({error:'Keyless endpoint must not receive Authorization'}));
    }
    if (request.method === 'GET' && request.url === '/v1/models') {
        response.writeHead(200, {'Content-Type':'application/json'});
        return response.end(JSON.stringify({data:[{id:model}]}));
    }
    if (request.method === 'GET' && request.url === '/requests') {
        response.writeHead(200, {'Content-Type':'application/json'});
        return response.end(JSON.stringify(calls));
    }
    if (request.method !== 'POST' || request.url !== '/v1/chat/completions') {
        response.writeHead(404); return response.end();
    }
    const chunks = [];
    for await (const chunk of request) chunks.push(chunk);
    const body = JSON.parse(Buffer.concat(chunks).toString());
    calls.push({model:body.model,stream:body.stream,authorization:false});
    if (body.model !== model) { response.writeHead(400); return response.end('wrong model ID'); }
    response.writeHead(200, {'Content-Type':'text/event-stream'});
    response.write(`data: ${JSON.stringify({choices:[{delta:{role:'assistant',content:'Local model connection verified.'},finish_reason:null}]})}\n\n`);
    response.end(`data: ${JSON.stringify({choices:[{delta:{},finish_reason:'stop'}],usage:{prompt_tokens:1,completion_tokens:1}})}\n\ndata: [DONE]\n\n`);
}).listen(9099, '0.0.0.0');
