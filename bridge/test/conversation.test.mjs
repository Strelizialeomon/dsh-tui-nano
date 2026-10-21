// Conversation host unit tests. The fake Agent/Session/Context mirror only the
// public surface S02 is allowed to use, so these tests catch accidental
// dependencies on official internals.
import test from 'node:test'
import assert from 'node:assert/strict'

import { ConversationService, validateContent } from '../conversation/service.mjs'

const ROOT = 'session-root'

function fakeContext() {
  const handlers = new Map()
  return {
    handlers,
    on(event, handler) {
      const list = handlers.get(event) ?? []
      list.push(handler)
      handlers.set(event, list)
      return () => {
        const current = handlers.get(event) ?? []
        handlers.set(event, current.filter((item) => item !== handler))
      }
    },
    emit(event, payload, second) {
      for (const handler of [...(handlers.get(event) ?? [])]) handler(payload, second)
    },
  }
}

function fakeSession(events = []) {
  return {
    seq: 7,
    events,
    snapshotEvents() {
      return this.events
    },
  }
}

function fakeAgent({ status = 'idle', nextTurn = [], nextStep = [] } = {}) {
  const agent = {
    id: ROOT,
    status,
    calls: { followup: [], steer: [], cancel: [], replace: [], remove: [], steerFromQueue: [] },
    inbox: {
      nextTurn,
      nextStep,
      replace(messageId, message) {
        agent.calls.replace.push({ messageId, message })
        agent.inbox.nextTurn = agent.inbox.nextTurn.map((item) => (item.id === messageId ? message : item))
        agent.inbox.nextStep = agent.inbox.nextStep.map((item) => (item.id === messageId ? message : item))
        return true
      },
      remove(messageId) {
        agent.calls.remove.push(messageId)
        agent.inbox.nextTurn = agent.inbox.nextTurn.filter((item) => item.id !== messageId)
        agent.inbox.nextStep = agent.inbox.nextStep.filter((item) => item.id !== messageId)
        return true
      },
    },
    followup(message) {
      agent.calls.followup.push(message)
      agent.inbox.nextTurn = [...agent.inbox.nextTurn, message]
    },
    steer(message) {
      agent.calls.steer.push(message)
      agent.inbox.nextStep = [...agent.inbox.nextStep, message]
    },
    cancel(cause, options) {
      agent.calls.cancel.push({ cause, options })
    },
  }
  return agent
}

function serviceFor({ status, nextTurn, nextStep, events } = {}) {
  const ctx = fakeContext()
  const agent = fakeAgent({ status, nextTurn, nextStep })
  const session = fakeSession(events)
  const published = []
  const hub = { publish: (kind, payload) => published.push({ kind, payload }) }
  const service = new ConversationService({ ctx, hub, agent, session, rootSessionId: ROOT })
  return { ctx, agent, session, published, service }
}

const address = { rootSessionId: ROOT }

test('submit queues an ordinary follow-up with official request identity', () => {
  const { agent, service } = serviceFor()
  const result = service.submit({
    address,
    requestId: 'req-a',
    delivery: 'queue',
    content: [{ type: 'text', text: 'hello' }],
  })
  assert.equal(result.accepted, true)
  assert.equal(result.duplicate, false)
  assert.equal(result.requestId, 'req-a')
  assert.match(result.messageId, /^[0-9a-f-]{36}$/)
  assert.equal(agent.calls.followup.length, 1)
  const message = agent.calls.followup[0]
  assert.equal(message.id, result.messageId)
  assert.equal(message.role, 'user')
  assert.deepEqual(message.content, [{ type: 'text', text: 'hello' }])
  assert.deepEqual(message.source, { kind: 'user', rpcId: 'req-a' })
  assert.ok(Object.isFrozen(message.source))
})

test('the same requestId never creates a second message', () => {
  const { agent, service } = serviceFor()
  const first = service.submit({ address, requestId: 'req-a', delivery: 'queue', content: [{ type: 'text', text: 'one' }] })
  const second = service.submit({ address, requestId: 'req-a', delivery: 'queue', content: [{ type: 'text', text: 'two' }] })
  assert.equal(second.duplicate, true)
  assert.equal(second.messageId, first.messageId)
  assert.equal(agent.calls.followup.length, 1)
})

test('a durable user message with the same rpcId is recognised after a restart', () => {
  const events = [
    { type: 'user/message', data: { id: 'durable-1', role: 'user', content: [], source: { kind: 'user', rpcId: 'req-a' } } },
  ]
  const { agent, service } = serviceFor({ events })
  const result = service.submit({ address, requestId: 'req-a', delivery: 'queue', content: [{ type: 'text', text: 'again' }] })
  assert.equal(result.duplicate, true)
  assert.equal(result.messageId, 'durable-1')
  assert.equal(agent.calls.followup.length, 0)
})

test('blank content is rejected with a distinguishable reason', () => {
  const { agent, service } = serviceFor()
  assert.throws(
    () => service.submit({ address, requestId: 'r', delivery: 'queue', content: [{ type: 'text', text: ' \n\t' }] }),
    (error) => error.code === 1012,
  )
  assert.equal(agent.calls.followup.length, 0)
})

test('non-text blocks are rejected instead of half-submitting attachments', () => {
  assert.throws(() => validateContent([{ type: 'image', attachment: {} }]), (error) => error.code === 1013)
  assert.throws(() => validateContent([]), (error) => error.code === 1012)
})

test('steer is only offered while the turn is running', () => {
  const idle = serviceFor({ status: 'idle' })
  assert.throws(
    () => idle.service.submit({ address, requestId: 's', delivery: 'steer', content: [{ type: 'text', text: 'x' }] }),
    (error) => error.code === 1016,
  )
  assert.equal(idle.agent.calls.steer.length, 0)

  const running = serviceFor({ status: 'running' })
  const result = running.service.submit({ address, requestId: 's', delivery: 'steer', content: [{ type: 'text', text: 'x' }] })
  assert.equal(result.accepted, true)
  assert.equal(running.agent.calls.steer.length, 1)
})

test('editQueued replaces text in place and rejects attachment items', () => {
  const queued = {
    id: 'm-1',
    role: 'user',
    content: [{ type: 'text', text: 'old' }],
    source: { kind: 'user', rpcId: 'r-1' },
  }
  const { agent, service } = serviceFor({ nextTurn: [queued] })
  const result = service.editQueued({ address, messageId: 'm-1', content: [{ type: 'text', text: 'new' }] })
  assert.deepEqual(result, { accepted: true, messageId: 'm-1' })
  assert.deepEqual(agent.calls.replace[0].message.content, [{ type: 'text', text: 'new' }])
  assert.equal(agent.calls.replace[0].message.id, 'm-1')
  assert.deepEqual(agent.calls.replace[0].message.source, { kind: 'user', rpcId: 'r-1' })

  const withAttachment = {
    id: 'm-2',
    role: 'user',
    content: [{ type: 'text', text: 'see' }, { type: 'image', attachment: {} }],
    source: { kind: 'user', rpcId: 'r-2' },
  }
  const second = serviceFor({ nextTurn: [withAttachment] })
  assert.throws(
    () => second.service.editQueued({ address, messageId: 'm-2', content: [{ type: 'text', text: 'x' }] }),
    (error) => error.code === 1015,
  )
})

test('editQueued and deleteQueued report a claimed item as missing', () => {
  const { service } = serviceFor()
  assert.throws(
    () => service.editQueued({ address, messageId: 'gone', content: [{ type: 'text', text: 'x' }] }),
    (error) => error.code === 1014,
  )
  assert.throws(
    () => service.deleteQueued({ address, messageId: 'gone' }),
    (error) => error.code === 1014,
  )
})

test('deleteQueued removes a pending item', () => {
  const queued = { id: 'm-1', role: 'user', content: [{ type: 'text', text: 'old' }], source: { kind: 'user' } }
  const { agent, service } = serviceFor({ nextTurn: [queued] })
  assert.deepEqual(service.deleteQueued({ address, messageId: 'm-1' }), { removed: true })
  assert.deepEqual(agent.calls.remove, ['m-1'])
})

test('steerQueued moves a next-turn item into steering while running', () => {
  const queued = { id: 'm-1', role: 'user', content: [{ type: 'text', text: 'steer me' }], source: { kind: 'user' } }
  const running = serviceFor({ status: 'running', nextTurn: [queued] })
  assert.deepEqual(running.service.steerQueued({ address, messageId: 'm-1' }), { accepted: true })
  assert.deepEqual(running.agent.calls.remove, ['m-1'])
  assert.equal(running.agent.calls.steer[0], queued)

  const idle = serviceFor({ status: 'idle', nextTurn: [queued] })
  assert.throws(() => idle.service.steerQueued({ address, messageId: 'm-1' }), (error) => error.code === 1016)

  const stepItem = serviceFor({ status: 'running', nextStep: [queued] })
  assert.throws(() => stepItem.service.steerQueued({ address, messageId: 'm-1' }), (error) => error.code === 1016)
})

test('cancel keeps the inbox and only asks the Agent to stop', () => {
  const { agent, service } = serviceFor({ status: 'running' })
  assert.deepEqual(service.cancel({ address }), { accepted: true })
  assert.deepEqual(agent.calls.cancel, [{ cause: { kind: 'user' }, options: { keepInbox: true } }])
})

test('snapshot returns the official baseline with the full inbox', () => {
  const queued = { id: 'm-1', role: 'user', content: [{ type: 'text', text: 'q' }], source: { kind: 'user' } }
  const { service } = serviceFor({ status: 'running', nextTurn: [queued] })
  const snapshot = service.snapshot({ address })
  assert.equal(snapshot.seq, 7)
  assert.equal(snapshot.status, 'running')
  assert.deepEqual(snapshot.inbox.nextTurn, [queued])
  assert.deepEqual(snapshot.inbox.nextStep, [])
})

test('child addresses and foreign roots are refused', () => {
  const { service } = serviceFor()
  assert.throws(
    () => service.snapshot({ address: { rootSessionId: ROOT, childSessionId: 'child' } }),
    (error) => error.code === 1008,
  )
  assert.throws(
    () => service.snapshot({ address: { rootSessionId: 'other' } }),
    (error) => error.code === 1011,
  )
})

test('oversized text is rejected before it reaches the official inbox', () => {
  const { agent, service } = serviceFor()
  const text = 'x'.repeat(32 * 1024 + 1)
  assert.throws(
    () => service.submit({ address, requestId: 'big', delivery: 'queue', content: [{ type: 'text', text }] }),
    (error) => error.code === 1018,
  )
  assert.equal(agent.calls.followup.length, 0)
})

test('a deleted queued item no longer dedupes a later submit', () => {
  const { service } = serviceFor()
  const first = service.submit({
    address,
    requestId: 'r-1',
    delivery: 'queue',
    content: [{ type: 'text', text: 'one' }],
  })
  service.deleteQueued({ address, messageId: first.messageId })
  const second = service.submit({
    address,
    requestId: 'r-1',
    delivery: 'queue',
    content: [{ type: 'text', text: 'two' }],
  })
  assert.equal(second.duplicate, false)
  assert.notEqual(second.messageId, first.messageId)
})

test('a discarded inbox item clears its accepted request id', () => {
  const { ctx, agent, service } = serviceFor()
  const first = service.submit({
    address,
    requestId: 'r-1',
    delivery: 'queue',
    content: [{ type: 'text', text: 'one' }],
  })
  agent.inbox.nextTurn = []
  ctx.emit('agent/inbox/discarded', {
    agent,
    message: { id: first.messageId, role: 'user', content: [], source: { kind: 'user', rpcId: 'r-1' } },
  })
  const second = service.submit({
    address,
    requestId: 'r-1',
    delivery: 'queue',
    content: [{ type: 'text', text: 'two' }],
  })
  assert.equal(second.duplicate, false)
})

test('a huge inbox projection is bounded and flagged truncated', () => {
  const huge = {
    id: 'm-big',
    role: 'user',
    content: [{ type: 'text', text: 'x'.repeat(300_000) }],
    source: { kind: 'user', rpcId: 'r-big' },
  }
  const { service } = serviceFor({ nextTurn: [huge] })
  const payload = service.inboxPayload()
  assert.equal(payload.truncated, true)
  assert.ok(Buffer.byteLength(JSON.stringify(payload), 'utf8') <= 262_144)
  assert.equal(payload.inbox.nextTurn[0].id, 'm-big')
})

test('live Agent events publish absolute conversation projections', () => {
  const { ctx, agent, published, service } = serviceFor({ status: 'idle' })
  ctx.emit('agent/status', { agent, status: 'running' })
  ctx.emit('agent/inbox/inserted', { agent, message: { id: 'm-1' } })
  ctx.emit('agent/assistant-stream', {
    agent,
    frame: { type: 'start', attemptId: 'a-1', revision: 1, turn: 1, step: 1 },
  })
  assert.deepEqual(published, [
    { kind: 'conversation.status', payload: { address, status: 'running' } },
    { kind: 'conversation.inbox', payload: { address, inbox: { nextTurn: [], nextStep: [] } } },
    {
      kind: 'conversation.stream',
      payload: { address, frame: { type: 'start', attemptId: 'a-1', revision: 1, turn: 1, step: 1 } },
    },
  ])
  service.close()
  ctx.emit('agent/status', { agent, status: 'idle' })
  assert.equal(published.length, 3)
})
