// S08 job observation: official projections only, non-consuming reads, the
// human stop reason, and a not-found answer that never guesses.
import test from 'node:test'
import assert from 'node:assert/strict'

import { JobsService } from '../jobs/service.mjs'

const ROOT = 'session-root'

function view(id = 'bash-0123456789abcdef') {
  return {
    id,
    kind: 'bash',
    label: 'sleep 30',
    owner: ROOT,
    status: 'running',
    startedAt: 1,
    output: { total: 12, earliest: 0 },
  }
}

function setup({ registry } = {}) {
  const published = []
  const subscriptions = []
  const handlers = {}
  const registryValue = registry ?? {
    events: {
      subscribe(filter, listener) {
        subscriptions.push({ filter, listener })
        return () => {}
      },
    },
    attachController() {
      return () => {}
    },
    list: () => [view()],
    readAt(id, from, caller) {
      return { chunks: [{ at: from, text: 'x' }], next: from + 1, lossy: false }
    },
    kill() {
      return 'requested'
    },
  }
  const ctx = {
    get(name) {
      return name === 'jobs' ? registryValue : undefined
    },
    on(event, handler) {
      handlers[event] = handler
      return () => {}
    },
  }
  const hub = {
    publish(kind, payload) {
      published.push({ kind, payload })
    },
  }
  return {
    published,
    subscriptions,
    handlers,
    service: new JobsService({ ctx, hub, session: {}, rootSessionId: ROOT }),
  }
}

test('a live registration links its durable tool call into the snapshot', () => {
  const { service, handlers } = setup()
  handlers['session/event'](
    { id: ROOT },
    {
      type: 'job/registered',
      data: {
        jobId: 'bash-0123456789abcdef',
        owner: ROOT,
        toolCall: { callId: 'c-1', rootCallId: 'r-1' },
      },
    },
  )
  const snapshot = service.snapshot({ address: { rootSessionId: ROOT } })
  assert.equal(snapshot.seq, 0)
  assert.equal(snapshot.jobs.length, 1)
  assert.deepEqual(snapshot.jobs[0].toolCall, { callId: 'c-1', rootCallId: 'r-1' })
  assert.equal(snapshot.jobs[0].status, 'running')
})

test('the snapshot sequence advances with live events', () => {
  const { service, subscriptions } = setup()
  assert.equal(service.snapshot({ address: { rootSessionId: ROOT } }).seq, 0)
  subscriptions[0].listener({ type: 'registered', job: view() })
  assert.equal(service.snapshot({ address: { rootSessionId: ROOT } }).seq, 1)
})

test('follow is a non-consuming readAt at the requested offset', () => {
  const calls = []
  const { service } = setup({
    registry: {
      events: { subscribe: () => () => {} },
      attachController: () => () => {},
      list: () => [],
      readAt(id, from, caller) {
        calls.push({ id, from, caller })
        return { chunks: [{ at: from, text: 'ab' }], next: from + 2, lossy: true }
      },
      kill() {
        return 'requested'
      },
    },
  })
  const output = service.follow({
    address: { rootSessionId: ROOT },
    jobId: 'bash-0123456789abcdef',
    from: 40,
  })
  assert.deepEqual(output, { chunks: [{ at: 40, text: 'ab' }], next: 42, lossy: true })
  assert.deepEqual(calls, [{ id: 'bash-0123456789abcdef', from: 40, caller: ROOT }])
})

test('kill forwards the human reason and reports only the request fact', () => {
  const calls = []
  const { service } = setup({
    registry: {
      events: { subscribe: () => () => {} },
      attachController: () => () => {},
      list: () => [],
      readAt: () => ({ chunks: [], next: 0, lossy: false }),
      kill(id, caller, reason) {
        calls.push({ id, caller, reason })
        return 'requested'
      },
    },
  })
  const result = service.kill({ address: { rootSessionId: ROOT }, jobId: 'bash-a' })
  assert.deepEqual(result, { outcome: 'requested' })
  assert.deepEqual(calls, [{ id: 'bash-a', caller: ROOT, reason: 'cancelled by the user' }])
})

test('an already-finished kill keeps its distinct answer', () => {
  const { service } = setup({
    registry: {
      events: { subscribe: () => () => {} },
      attachController: () => () => {},
      list: () => [],
      readAt: () => ({ chunks: [], next: 0, lossy: false }),
      kill: () => 'already-finished',
    },
  })
  assert.deepEqual(service.kill({ address: { rootSessionId: ROOT }, jobId: 'bash-a' }), {
    outcome: 'already-finished',
  })
})

test('unknown and foreign jobs are reported as not-found, not already-finished', () => {
  const { service } = setup({
    registry: {
      events: { subscribe: () => () => {} },
      attachController: () => () => {},
      list: () => [],
      readAt() {
        throw new Error('unknown job bash-a')
      },
      kill() {
        throw new Error('job bash-a belongs to another session')
      },
    },
  })
  assert.throws(
    () => service.follow({ address: { rootSessionId: ROOT }, jobId: 'bash-a', from: 0 }),
    (error) => error.rpcName === 'nano/jobs/not-found',
  )
  assert.throws(
    () => service.kill({ address: { rootSessionId: ROOT }, jobId: 'bash-a' }),
    (error) => error.rpcName === 'nano/jobs/not-found',
  )
})

test('a producer cancel throw stays an internal error, not not-found', () => {
  const { service } = setup({
    registry: {
      events: { subscribe: () => () => {} },
      list: () => [],
      readAt: () => ({ chunks: [], next: 0, lossy: false }),
      kill() {
        throw new Error('producer cancel failed')
      },
    },
  })
  assert.throws(
    () => service.kill({ address: { rootSessionId: ROOT }, jobId: 'bash-a' }),
    (error) =>
      error.rpcName === 'jsonrpc/internal-error' &&
      /producer cancel failed/.test(error.message),
  )
})

test('official registry events are republished without inventing status', () => {
  const { service, published, subscriptions } = setup()
  assert.equal(subscriptions.length, 1)
  assert.deepEqual(subscriptions[0].filter, { owner: ROOT })
  subscriptions[0].listener({ type: 'registered', job: view() })
  subscriptions[0].listener({
    type: 'settled',
    cause: 'producer',
    awaited: false,
    job: { ...view(), status: 'completed', finishedAt: 2 },
  })
  subscriptions[0].listener({ type: 'output', id: 'bash-0123456789abcdef', owner: ROOT, total: 12 })
  assert.equal(published[0].kind, 'jobs.event')
  assert.equal(published[0].payload.type, 'registered')
  assert.equal(published[1].payload.cause, 'producer')
  assert.equal(published[1].payload.awaited, false)
  assert.equal(published[2].kind, 'jobs.output')
  assert.equal(published[2].payload.total, 12)
})

test('an absent registry is unsupported rather than an empty task list', () => {
  const ctx = { get: () => undefined, on: () => () => {} }
  const hub = { publish() {} }
  const service = new JobsService({
    ctx,
    hub,
    session: { snapshotEvents: () => [] },
    rootSessionId: ROOT,
  })
  assert.throws(
    () => service.snapshot({ address: { rootSessionId: ROOT } }),
    (error) => error.rpcName === 'nano/unsupported',
  )
})

test('a foreign address is refused', () => {
  const { service } = setup()
  assert.throws(
    () => service.snapshot({ address: { rootSessionId: 'other' } }),
    (error) => error.rpcName === 'nano/conversation/address',
  )
})

test('an invalid offset is an invalid parameter, not a not-found read', () => {
  const { service } = setup()
  for (const from of [-1, Number.MAX_SAFE_INTEGER + 1, 1.5]) {
    assert.throws(
      () => service.follow({ address: { rootSessionId: ROOT }, jobId: 'bash-a', from }),
      (error) => error.rpcName === 'jsonrpc/invalid-params',
    )
  }
})
