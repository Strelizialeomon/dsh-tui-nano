// S12 subagent host: official catalog reads, mode resolution, human
// continuation, interrupt, and lifecycle publication. The fakes expose only
// the official service surface the bridge is allowed to call.
import test from 'node:test'
import assert from 'node:assert/strict'
import fs from 'node:fs'

import { SubagentsService } from '../subagents/service.mjs'

const ROOT = 'root'

function childFor(parent) {
  return parent === ROOT ? 'child' : 'nested'
}

function fakeHub() {
  const published = []
  return {
    published,
    publish(kind, payload) {
      published.push({ kind, payload })
    },
  }
}

function fakeCtx(options = {}) {
  const listeners = []
  const services = {
    subagents: options.subagents,
    sessionQuery: options.query,
    attachments: options.attachments,
    ...options.services,
  }
  return {
    listeners,
    services,
    agents: {
      get: options.getAgent ?? (() => undefined),
    },
    get(name) {
      return this.services[name]
    },
    on(event, handler) {
      listeners.push({ event, handler })
      return () => {
        const index = listeners.findIndex((entry) => entry.handler === handler)
        if (index >= 0) listeners.splice(index, 1)
      }
    },
    emit(event, ...args) {
      for (const entry of [...listeners]) {
        if (entry.event === event) entry.handler(...args)
      }
    },
  }
}

function observation(values, events = [], cursor = 4) {
  const lease = {
    cursor,
    header: { id: 'child', createdAt: 1 },
    events,
    projections: { asOfSeq: cursor, values },
    [Symbol.dispose]() {
      lease.disposed = true
    },
    disposed: false,
  }
  return lease
}

function serviceFor({ subagents, query, attachments, getAgent } = {}) {
  const ctx = fakeCtx({ subagents, query, attachments, getAgent })
  const hub = fakeHub()
  const service = new SubagentsService({
    ctx,
    hub,
    agent: { id: ROOT },
    session: { id: ROOT, seq: 10 },
    rootSessionId: ROOT,
    attachments,
  })
  return { ctx, hub, service }
}

test('snapshot reports direct children with official activity and timing', async () => {
  const observed = []
  const subagents = {
    async listChildren() {
      return [
        { id: 'a', createdAt: 1, mode: 'continuable', label: 'alpha' },
        { id: 'b', createdAt: 2, mode: 'external' },
      ]
    },
  }
  const query = {
    async observeSession(id) {
      observed.push(id)
      return observation({
        subagent: { mode: 'continuable', label: 'alpha' },
        subagentTiming: { settledMs: 1500, active: { since: 10, through: 20 }, lastTurnCompleted: true },
      })
    },
  }
  const { service } = serviceFor({
    subagents,
    query,
    getAgent: (id) => (id === 'a' ? { status: 'running' } : undefined),
  })
  const snapshot = await service.snapshot()
  assert.equal(snapshot.parentLive, true)
  assert.deepEqual(snapshot.agents.map((row) => row.id), ['a', 'b'])
  assert.equal(snapshot.agents[0].activity, 'running')
  assert.equal(snapshot.agents[0].lastTurnCompleted, true)
  assert.equal(snapshot.agents[0].settledMs, 1500)
  assert.equal(snapshot.agents[0].activeSince, 10)
  assert.equal(snapshot.agents[1].activity, 'inactive')
  // External rows have no local session and never trigger a log read.
  assert.deepEqual(observed, ['a'])
})

test('snapshot keeps membership when a child projection is unreadable', async () => {
  const subagents = {
    async listChildren() {
      return [{ id: 'a', createdAt: 1, mode: 'one-shot' }]
    },
  }
  const query = {
    async observeSession() {
      throw new Error('log is corrupt')
    },
  }
  const { service } = serviceFor({ subagents, query })
  const snapshot = await service.snapshot()
  assert.equal(snapshot.agents.length, 1)
  assert.equal(snapshot.diagnostics[0].reason, 'log is corrupt')
})

test('list preserves recursive rows and branch diagnostics', async () => {
  const subagents = {
    async listDescendants() {
      return [
        {
          kind: 'child',
          id: 'a',
          parentId: ROOT,
          depth: 1,
          mode: 'continuable',
          label: 'alpha',
          activity: 'running',
          hasChildren: true,
        },
        { kind: 'diagnostic', id: 'broken', parentId: 'a', depth: 2, reason: 'corrupt' },
        {
          kind: 'child',
          id: 'c',
          parentId: 'a',
          depth: 2,
          mode: 'one-shot',
          activity: 'inactive',
          hasChildren: false,
        },
      ]
    },
  }
  const { service } = serviceFor({ subagents })
  const list = await service.list()
  assert.deepEqual(list.entries.map((row) => [row.id, row.depth]), [
    ['a', 1],
    ['c', 2],
  ])
  assert.equal(list.diagnostics[0].reason, 'corrupt')
  assert.equal(list.diagnostics[0].parentId, 'a')
})

test('describe resolves an unknown mode from the child projection', async () => {
  const subagents = {
    async listChildren() {
      return [{ id: 'child', createdAt: 1, mode: 'unknown' }]
    },
  }
  const query = {
    async observeSession() {
      return observation({
        subagent: { mode: 'continuable', label: 'real task' },
        subagentTiming: { settledMs: 5, lastTurnCompleted: false },
      })
    },
  }
  const { service } = serviceFor({ subagents, query })
  const described = await service.describe({ parentSessionId: ROOT, childSessionId: 'child' })
  assert.equal(described.mode, 'continuable')
  assert.equal(described.label, 'real task')
  assert.equal(described.parentLive, true)
  assert.equal(described.settledMs, 5)
})

test('describe rejects a session that is not a direct child', async () => {
  const subagents = {
    async listChildren() {
      return []
    },
  }
  const { service } = serviceFor({ subagents })
  await assert.rejects(
    service.describe({ parentSessionId: ROOT, childSessionId: 'other' }),
    (error) => error.rpcName === 'nano/subagents/unauthorized',
  )
})

test('prompt uses the official continuation path and rejects generic files', async () => {
  const received = []
  const subagents = {
    async listChildren() {
      return [{ id: 'child', createdAt: 1, mode: 'continuable' }]
    },
    async prompt(request) {
      received.push(request)
      return { messageId: 'message-1' }
    },
  }
  const query = {
    async observeSession() {
      return observation({ subagent: { mode: 'continuable', label: 'child' } })
    },
  }
  const commitments = []
  const attachments = {
    resolveSubagentPrompt(address, content, requestId) {
      assert.equal(address.childSessionId, 'child')
      assert.equal(requestId, 'req-1')
      const receipts = content.some((block) => block.type === 'image') ? [{ id: 'r' }] : []
      return {
        parts: content.map((block) =>
          block.type === 'image'
            ? { type: 'image', mediaType: 'image/png', data: 'aGk=' }
            : block,
        ),
        receipts,
      }
    },
    commit(requestId, receipts) {
      commitments.push(['commit', requestId, receipts.length])
    },
    releasePrompt(requestId, receipts) {
      commitments.push(['release', requestId, receipts.length])
    },
  }
  const { service } = serviceFor({ subagents, query, attachments })
  const address = {
    rootSessionId: ROOT,
    parentSessionId: ROOT,
    childSessionId: 'child',
    mode: 'continuable',
  }
  const receipt = await service.prompt({
    address,
    requestId: 'req-1',
    delivery: 'queue',
    content: [{ type: 'text', text: 'hello' }],
  })
  assert.deepEqual(receipt, { accepted: true, messageId: 'message-1' })
  assert.equal(received[0].parentSessionId, ROOT)
  assert.equal(received[0].childSessionId, 'child')
  assert.equal(received[0].mode, 'continuable')
  assert.deepEqual(commitments, [['commit', 'req-1', 0]])
})

test('prompt releases receipt bindings when the official call rejects', async () => {
  const subagents = {
    async listChildren() {
      return [{ id: 'child', createdAt: 1, mode: 'continuable' }]
    },
    async prompt() {
      const error = new Error('capacity is full')
      error.code = 'ACTIVATION_LIMIT_REACHED'
      throw error
    },
  }
  const query = {
    async observeSession() {
      return observation({ subagent: { mode: 'continuable', label: 'child' } })
    },
  }
  const calls = []
  const attachments = {
    resolveSubagentPrompt() {
      return { parts: [{ type: 'text', text: 'hi' }], receipts: [{ id: 'r' }] }
    },
    commit() {
      calls.push('commit')
    },
    releasePrompt() {
      calls.push('release')
    },
  }
  const { service } = serviceFor({ subagents, query, attachments })
  await assert.rejects(
    service.prompt({
      address: {
        rootSessionId: ROOT,
        parentSessionId: ROOT,
        childSessionId: 'child',
        mode: 'continuable',
      },
      requestId: 'req-1',
      delivery: 'steer',
      content: [{ type: 'text', text: 'hi' }],
    }),
    (error) => error.rpcName === 'nano/subagents/capacity',
  )
  assert.deepEqual(calls, ['release'])
})

test('prompt refuses a non-continuable address before calling the host', async () => {
  const subagents = {
    async listChildren() {
      return [{ id: 'child', createdAt: 1, mode: 'one-shot' }]
    },
  }
  const { service } = serviceFor({ subagents })
  await assert.rejects(
    service.prompt({
      address: {
        rootSessionId: ROOT,
        parentSessionId: ROOT,
        childSessionId: 'child',
        mode: 'one-shot',
      },
      requestId: 'req-1',
      delivery: 'queue',
      content: [{ type: 'text', text: 'hi' }],
    }),
    (error) => error.rpcName === 'nano/subagents/not-resumable',
  )
})

test('interrupt uses interruptByParent and never dispose', async () => {
  const calls = []
  const subagents = {
    interruptByParent(child, parent, mode) {
      calls.push({ child, parent, mode })
      return { accepted: true }
    },
  }
  const { service } = serviceFor({ subagents })
  const receipt = await service.interrupt({
    address: {
      rootSessionId: ROOT,
      parentSessionId: ROOT,
      childSessionId: 'child',
      mode: 'continuable',
    },
  })
  assert.deepEqual(receipt, { accepted: true })
  assert.deepEqual(calls, [{ child: 'child', parent: ROOT, mode: 'continuable' }])
})

test('lifecycle edges publish the official stop reason', () => {
  const { ctx, hub } = serviceFor({ subagents: {} })
  ctx.emit('subagent/start', { id: 'child', provider: 'spawn', local: true })
  ctx.emit('subagent/end', {
    id: 'child',
    provider: 'spawn',
    local: true,
    stopReason: 'max-tokens',
  })
  const events = hub.published.filter((entry) => entry.kind === 'subagents.event')
  assert.equal(events[0].payload.type, 'start')
  assert.equal(events[0].payload.childId, 'child')
  assert.equal(events[1].payload.type, 'end')
  assert.equal(events[1].payload.stopReason, 'max-tokens')

  ctx.emit('subagent/end', { id: 'child', provider: 'x', local: false, stopReason: 'future' })
  const last = hub.published.at(-1)
  assert.equal(last.payload.stopReason, 'unknown')
})

test('assistant frames for known children use the child address', () => {
  const subagents = {}
  const { ctx, hub } = serviceFor({
    subagents,
    getAgent: (id) =>
      id === 'child'
        ? { session: { header: { parentSession: ROOT } } }
        : undefined,
  })
  ctx.emit('subagent/start', { id: 'child', provider: 'spawn', local: true })
  ctx.emit('agent/assistant-stream', {
    agent: { id: 'child' },
    frame: { frame: 'chunk' },
  })
  const stream = hub.published.find((entry) => entry.kind === 'conversation.stream')
  assert.equal(stream.payload.address.childSessionId, 'child')
  assert.equal(stream.payload.address.parentSessionId, ROOT)
  assert.equal(stream.payload.address.mode, 'continuable')
})

test('a historical one-shot descriptor fixture stays read-only', async () => {
  const fixture = JSON.parse(
    fs.readFileSync(new URL('./fixtures/subagent-one-shot.json', import.meta.url), 'utf8'),
  )
  const subagents = {
    async listChildren() {
      return [{ id: 'finished', createdAt: 1, mode: 'one-shot', label: fixture.descriptor.label }]
    },
  }
  const query = {
    async observeSession() {
      return observation(
        { subagent: fixture.descriptor },
        fixture.events,
        fixture.events.length - 1,
      )
    },
  }
  const { service } = serviceFor({ subagents, query })
  const described = await service.describe({
    parentSessionId: ROOT,
    childSessionId: 'finished',
  })
  assert.equal(described.mode, 'one-shot')
  assert.equal(described.label, 'finished task')
  // The replay path sees the recorded descriptor/history, not live input.
  const replay = await query.observeSession('finished')
  assert.deepEqual(
    replay.events.map((event) => event.type),
    ['subagent/descriptor', 'user/message', 'assistant/message', 'turn/end'],
  )
})

test('describe and interrupt refuse a parent outside this host root', async () => {
  const subagents = {
    async listDescendants() {
      return [{ kind: 'child', id: 'reachable', parentId: ROOT, depth: 1, mode: 'continuable', activity: 'inactive', hasChildren: false }]
    },
    async listChildren(parent) {
      return [{ id: childFor(parent), createdAt: 1, mode: 'continuable' }]
    },
    interruptByParent() {
      throw new Error('must not be called')
    },
  }
  const { service } = serviceFor({ subagents })
  await assert.rejects(
    service.describe({ parentSessionId: 'foreign', childSessionId: 'child' }),
    (error) => error.rpcName === 'nano/subagents/unauthorized',
  )
  await assert.rejects(
    service.interrupt({
      address: {
        rootSessionId: ROOT,
        parentSessionId: 'foreign',
        childSessionId: 'child',
        mode: 'continuable',
      },
    }),
    (error) => error.rpcName === 'nano/subagents/unauthorized',
  )
})

test('capacity refusals map to the capacity error', async () => {
  const subagents = {
    async listChildren() {
      return [{ id: 'child', createdAt: 1, mode: 'continuable' }]
    },
    async prompt() {
      const cause = new Error('limit')
      cause.code = 'ACTIVATION_LIMIT_REACHED'
      const error = new Error('temporarily unavailable')
      error.code = 'subagent/delivery-unavailable'
      error.cause = cause
      throw error
    },
  }
  const query = {
    async observeSession() {
      return observation({ subagent: { mode: 'continuable', label: 'child' } })
    },
  }
  const attachments = {
    resolveSubagentPrompt() {
      return { parts: [{ type: 'text', text: 'hi' }], receipts: [] }
    },
    commit() {},
    releasePrompt() {},
  }
  const { service } = serviceFor({ subagents, query, attachments })
  await assert.rejects(
    service.prompt({
      address: {
        rootSessionId: ROOT,
        parentSessionId: ROOT,
        childSessionId: 'child',
        mode: 'continuable',
      },
      requestId: 'req-1',
      delivery: 'queue',
      content: [{ type: 'text', text: 'hi' }],
    }),
    (error) => error.rpcName === 'nano/subagents/capacity',
  )
})

test('authorizeChild checks the parent catalog and mode agreement', async () => {
  const subagents = {
    async listChildren(parent) {
      assert.equal(parent, ROOT)
      return [{ id: 'child', createdAt: 1, mode: 'continuable' }]
    },
  }
  const { service } = serviceFor({ subagents })
  const authorized = await service.authorizeChild({
    rootSessionId: ROOT,
    parentSessionId: ROOT,
    childSessionId: 'child',
    mode: 'unknown',
  })
  assert.equal(authorized.mode, 'unknown')
  await assert.rejects(
    service.authorizeChild({
      rootSessionId: ROOT,
      parentSessionId: ROOT,
      childSessionId: 'child',
      mode: 'one-shot',
    }),
    (error) => error.rpcName === 'nano/subagents/invalid',
  )
})
