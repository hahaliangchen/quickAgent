import assert from 'node:assert/strict'
import { readFile } from 'node:fs/promises'
import test from 'node:test'
import ts from 'typescript'
const source = await readFile(new URL('../src/model.ts', import.meta.url), 'utf8')
const { outputText } = ts.transpileModule(source, { compilerOptions: { module: ts.ModuleKind.ESNext, target: ts.ScriptTarget.ES2024 } })
const { latestPlan, groupPlanSteps } = await import('data:text/javascript;base64,' + Buffer.from(outputText).toString('base64'))
const summarySource = await readFile(new URL('../src/components/toolSummary.ts', import.meta.url), 'utf8')
const { outputText: summaryOutput } = ts.transpileModule(summarySource, { compilerOptions: { module: ts.ModuleKind.ESNext, target: ts.ScriptTarget.ES2024 } })
const { summaryFor } = await import('data:text/javascript;base64,' + Buffer.from(summaryOutput).toString('base64'))

const top = (id, status) => ({ id, title: id, status })
const child = (id, status, parent_id) => ({ id, title: id, status, parent_id })

test('a flat snapshot folds into one level of sub-steps and keeps the published order', () => {
  const tree = groupPlanSteps([
    top('a', 'in_progress'),
    child('a1', 'done', 'a'),
    child('a2', 'pending', 'a'),
    top('b', 'pending'),
  ])
  assert.deepEqual(tree.topSteps.map(step => step.id), ['a', 'b'])
  assert.deepEqual(tree.topSteps[0].children.map(step => step.id), ['a1', 'a2'])
  assert.deepEqual(tree.topSteps[1].children, [])
  assert.equal(tree.topTotal, 2)
  assert.equal(tree.topCompleted, 0)
})

test('a broken parent link degrades to a top-level step instead of hiding the item', () => {
  const orphan = groupPlanSteps([top('a', 'pending'), child('lost', 'pending', 'ghost')])
  assert.deepEqual(orphan.topSteps.map(step => step.id), ['a', 'lost'])
  assert.equal(orphan.topTotal, 2)
  const selfLinked = groupPlanSteps([child('loop', 'pending', 'loop')])
  assert.deepEqual(selfLinked.topSteps.map(step => step.id), ['loop'])
  // A sub-step of a sub-step would be a third level: it renders beside its parent.
  const deep = groupPlanSteps([top('p', 'pending'), child('c', 'pending', 'p'), child('gc', 'pending', 'c')])
  assert.deepEqual(deep.topSteps.map(step => step.id), ['p', 'gc'])
  assert.deepEqual(deep.topSteps[0].children.map(step => step.id), ['c'])
})

test('progress is weighted per top-level step and refined by its sub-steps', () => {
  const tree = groupPlanSteps([
    top('shipped', 'done'),
    top('build', 'in_progress'),
    child('b1', 'done', 'build'),
    child('b2', 'done', 'build'),
    child('b3', 'pending', 'build'),
    child('b4', 'blocked', 'build'),
    top('untouched', 'pending'),
  ])
  // 1 (shipped) + 2/4 (build) + 0 (untouched) over three top-level steps.
  assert.equal(tree.percent, 50)
  assert.equal(tree.topCompleted, 1)
  // One done sub-step out of four lifts its phase to a quarter of the weight.
  const quarter = groupPlanSteps([top('a', 'pending'), child('a1', 'done', 'a'), child('a2', 'pending', 'a'), child('a3', 'pending', 'a'), child('a4', 'pending', 'a')])
  assert.equal(quarter.percent, 25)
  const running = groupPlanSteps([top('a', 'in_progress'), child('a1', 'done', 'a'), child('a2', 'done', 'a')])
  assert.equal(running.percent, 100)
  // A parent marked done is complete even when a sub-step was never closed.
  const closed = groupPlanSteps([top('a', 'done'), child('a1', 'pending', 'a')])
  assert.equal(closed.percent, 100)
  assert.equal(closed.topCompleted, 1)
  assert.equal(groupPlanSteps([]).percent, 0)
})

test('latestPlan accepts sub-steps and counts only top-level steps as complete', () => {
  const plan = { type: 'plan/updated', data: { turn: 1, steps: [
    { id: 'a', title: 'Design', status: 'in_progress' },
    { id: 'a1', title: 'Sketch', status: 'done', parent_id: 'a' },
    { id: 'b', title: 'Build', status: 'done' },
  ] } }
  const latest = latestPlan([plan])
  assert.equal(latest.completed, 1)
  assert.equal(latest.steps[1].parent_id, 'a')
  const corrupt = { type: 'plan/updated', data: { steps: [{ id: 'a', title: 'A', status: 'done', parent_id: 7 }] } }
  assert.equal(latestPlan([plan, corrupt]).completed, 1)
})

test('the added-substeps call summarises the parent and the batch size', () => {
  assert.equal(summaryFor('plan_add_substeps', { parent_id: 'a', steps: [{ id: 'a1' }, { id: 'a2' }] }), 'a · 2 项子步骤')
  assert.equal(summaryFor('plan_add_substeps', { parent_id: 'phase-1', steps: [] }), 'phase-1 · 0 项子步骤')
})