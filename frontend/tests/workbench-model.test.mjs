import assert from 'node:assert/strict'
import {readFile} from 'node:fs/promises'
import test from 'node:test'
import ts from 'typescript'
const source = await readFile(new URL('../src/workbenchModel.ts', import.meta.url),'utf8')
const {outputText} = ts.transpileModule(source,{compilerOptions:{module:ts.ModuleKind.ESNext,target:ts.ScriptTarget.ES2024}})
const {sessionGroups,statusOf,turnTone,turnWindow,callName,duration} = await import('data:text/javascript;base64,'+Buffer.from(outputText).toString('base64'))
const task = (id, extra={}) => ({task_id:id,prompt:id,status:'completed',updated_at:1,parent_task_id:null,...extra})
const run = (status, extra={}) => ({seq:1,status,rewritten_by:null,calls:[],rejected:[],...extra})

test('sessions group by project, by the plugin that owns them, and plain chats; draft conversations stay out', () => {
  const tasks=[task('a',{workspace:'/p/one',updated_at:3}),task('b',{workspace:'/p/two',updated_at:5}),task('old',{workspace:null,updated_at:1}),
    task('ppt',{plugin_id:'ppt-master',kind:'chat',updated_at:4}),task('flow',{plugin_id:'code-flow',kind:'chat',updated_at:2}),task('chat',{kind:'chat'}),
    task('draft-chat',{workspace:'/root/.codex-workspace-mcp/workflow-drafts/x'}),task('child',{parent_task_id:'a'}),task('archived',{archived:true}),task('blank',{prompt:'',status:'draft'})]
  const packages=[{id:'ppt-master',title:'PPT Master',origin:'external'},{id:'code-flow',title:'代码 Flow',origin:'builtin'}]
  const groups=sessionGroups(tasks,[{path:'/p/two',title:'Two'},{path:'/p/one',title:'One'}],packages,'/p/one',new Set(['draft-chat']),'')
  assert.deepEqual(groups.projects.map(g=>[g.title,g.tasks.map(t=>t.task_id)]),[['Two',['b']],['One',['a','old']]])
  assert.deepEqual(groups.plugins.map(g=>[g.title,g.detail,g.tasks.map(t=>t.task_id)]),[['PPT Master','外部插件',['ppt']],['代码 Flow','内置插件',['flow']]])
  assert.deepEqual(groups.chats.map(t=>t.task_id),['chat'])
  assert.deepEqual(sessionGroups(tasks,[],packages,'/p/one',new Set(),'ppt master').plugins.map(g=>g.tasks.map(t=>t.task_id)),[['ppt']],'a plugin session is found by its plugin name')
})

test('a run handed back after an error reads differently once its rewrite exists', () => {
  assert.equal(statusOf(run('rewritten')).label,'出错 · 已交回 AI')
  assert.equal(statusOf(run('rewritten',{rewritten_by:9})).label,'出错 · 已改写')
  assert.equal(statusOf(run('unknown')).label,'结果未记录')
  assert.equal(turnTone({programs:[run('rewritten',{rewritten_by:2}),run('completed')]}),'ok','a turn is where its last run ended')
  assert.equal(turnTone({programs:[run('failed')]}),'error')
})

test('turn tabs stay few however long the session is', () => {
  assert.deepEqual(turnWindow(3,1),[0,1,2])
  assert.deepEqual(turnWindow(27,10),[0,'gap',8,9,10,11,12,'gap',26])
  assert.deepEqual(turnWindow(27,0),[0,1,2,3,4,5,'gap',26])
  assert.deepEqual(turnWindow(27,26),[0,'gap',21,22,23,24,25,26])
  for (let selected=0; selected<27; selected++) assert.ok(turnWindow(27,selected).includes(selected))
  assert.deepEqual(turnWindow(26,12,5),[0,'gap',11,12,13,'gap',25],'the workbench shows five tabs at most')
  for (let selected=0; selected<26; selected++) assert.ok(turnWindow(26,selected,5).filter(item=>item!=='gap').length<=5)
})

test('names and durations read as the program wrote them', () => {
  assert.equal(callName('ppt_master__apply_html'),'apply_html')
  assert.equal(callName('designPage'),'designPage')
  assert.equal(duration(69_000),'1 分 9 秒')
  assert.equal(duration(42_000),'42 秒')
})
