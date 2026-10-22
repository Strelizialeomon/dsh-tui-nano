// S10 monitor behavior: non-consuming offsets, line assembly, filtering,
// merge windows, budgets, delivery receipts, and UI-only stream facts.
import test from 'node:test'
import assert from 'node:assert/strict'

import { MonitorService, buildNotification } from '../monitor/service.mjs'

const ROOT = 'session-root'
const JOB = 'bash-0123456789abcdef'

/** Deterministic clock for merge windows and delivery timeouts. */
function clock() {
  let now = 0
  let nextId = 1
  const tasks = new Map()
  return {
    now: () => now,
    setTimeout(fn, ms) {
      const id = nextId++
      tasks.set(id, { at: now + Math.max(0, ms), fn })
      return id
    },
    clearTimeout(id) {
      tasks.delete(id)
    },
    advance(ms) {
      const target = now + ms
      while (true) {
        let due
        for (const [id, task] of tasks) {
          if (task.at <= target && (due === undefined || task.at < due.task.at)) {
            due = { id, task }
          }
        }
        if (due === undefined) break
        tasks.delete(due.id)
        now = due.task.at
        due.task.fn()
      }
      now = target
    },
    pending: () => tasks.size,
  }
}

/** A mutable fake job registry with at-driven reads. */
function fakeRegistry({ job, text = '', chunks } = {}) {
  const base = {
    id: JOB,
    kind: 'bash',
    label: 'sleep 30',
    owner: ROOT,
    status: 'running',
    startedAt: 1,
    output: { total: 0, earliest: 0 },
    ...job,
  }
  const state = {
    job: base,
    bytes: Buffer.from(text, 'utf8'),
    chunks: chunks ?? [],
    listeners: [],
    reads: [],
    kills: [],
  }
  const registry = {
    events: {
      subscribe(filter, listener) {
        state.listeners.push({ filter, listener })
        return () => {}
      },
    },
    get(id) {
      if (id !== JOB) throw new Error(`unknown job ${id}`)
      const total = state.bytes.length > 0 ? state.bytes.length : (state.job.output?.total ?? 0)
      return { ...state.job, output: { ...state.job.output, total } }
    },
    readAt(id, from) {
      state.reads.push(from)
      if (id !== JOB) throw new Error(`unknown job ${id}`)
      if (state.chunks.length > 0) {
        const available = state.chunks.filter((chunk) => chunk.at + Buffer.byteLength(chunk.text, 'utf8') > from)
        const next = available.length === 0 ? state.bytes.length : Math.max(...available.map((chunk) => chunk.at + Buffer.byteLength(chunk.text, 'utf8')))
        return { chunks: available.map((chunk) => ({ ...chunk })), next: Math.max(next, from), lossy: state.lossy === true }
      }
      // Produce whole retained output from `from` as one chunk.
      const sliced = state.bytes.subarray(from).toString('utf8')
      return { chunks: sliced === '' ? [] : [{ at: from, text: sliced }], next: state.bytes.length, lossy: state.lossy === true }
    },
    kill(...args) {
      state.kills.push(args)
      return 'requested'
    },
  }
  return { state, registry }
}

function setup({ job, text, chunks, budgets } = {}) {
  const time = clock()
  const { state, registry } = fakeRegistry({ job, text, chunks })
  const published = []
  const followups = []
  const injects = []
  const handlers = {}
  const ctx = {
    get(name) {
      return name === 'jobs' ? registry : undefined
    },
    on(event, handler) {
      handlers[event] = handler
      return () => {}
    },
  }
  const service = new MonitorService({
    ctx,
    hub: { publish(kind, payload) { published.push({ kind, payload }) } },
    agent: {
      followup(message) { followups.push(message) },
      inject(message) { injects.push(message) },
    },
    session: {},
    rootSessionId: ROOT,
    timers: {
      setTimeout: time.setTimeout,
      clearTimeout: time.clearTimeout,
      now: time.now,
    },
  })
  const address = { rootSessionId: ROOT }
  // Zero min interval keeps the merge-window assertions independent from the
  // production 1s throttle; tests that need the throttle override it.
  const effectiveBudgets = { minIntervalMs: 0, ...(budgets ?? {}) }
  const create = (extra = {}) =>
    service.create({
      address,
      jobId: JOB,
      from: 'new',
      mode: 'notify',
      budgets: effectiveBudgets,
      ...extra,
    })
  const emit = (event) => {
    for (const listener of state.listeners) listener.listener(event)
  }
  const output = (total) => emit({ type: 'output', id: JOB, owner: ROOT, total })
  const settled = (status, detail) =>
    emit({
      type: 'settled',
      cause: 'producer',
      awaited: false,
      job: { ...state.job, status, detail, output: { ...state.job.output, total: state.bytes.length } },
    })
  const list = () => service.list({ address }).monitors[0]
  const insertion = (message, event = 'inserted') =>
    handlers[`agent/inbox/${event}`]({ agent: { id: ROOT }, message })
  const durable = (message) =>
    handlers['session/event']({ id: ROOT }, { type: 'user/message', data: message })
  return { service, state, registry, published, followups, injects, handlers, time, address, create, emit, output, settled, list, insertion, durable }
}

test('a new monitor starts at the current total and reports skipped bytes', () => {
  const { create, list, published } = setup({ job: { output: { total: 100, earliest: 10 } } })
  const result = create()
  assert.equal(result.existing, false)
  const monitor = result.monitor
  assert.equal(monitor.from, 'new')
  assert.equal(monitor.initialOffset, 100)
  assert.equal(monitor.offset, 100)
  assert.equal(monitor.unreadBytes, 90)
  assert.equal(monitor.skippedBytes, 90)
  assert.equal(list().offset, 100)
  assert.ok(published.some((event) => event.kind === 'monitor.updated'))
})

test('earliest replays the retained head exactly once', () => {
  const { create, list, followups, time } = setup({
    job: { status: 'completed', output: { total: 10, earliest: 0 } },
    text: 'line one\nline two\n',
  })
  const result = create({ from: 'earliest' })
  assert.equal(result.monitor.initialOffset, 0)
  time.advance(1000)
  assert.equal(followups.length, 1)
  const text = followups[0].content[0].text
  assert.match(text, /first@0: line one/)
  assert.match(text, /last@9: line two/)
  assert.equal(list().stream, 'stream-ended')
  assert.equal(list().listening, false)
})

test('a line split across chunks is assembled before notification', () => {
  const { create, state, output, time, followups } = setup({
    chunks: [
      { at: 0, text: 'hel' },
      { at: 3, text: 'lo\nworld\n' },
    ],
  })
  create()
  state.chunks = [
    { at: 0, text: 'hel' },
    { at: 3, text: 'lo\nworld\n' },
  ]
  state.bytes = Buffer.from('hello\nworld\n')
  output(12)
  time.advance(800)
  assert.equal(followups.length, 1)
  const text = followups[0].content[0].text
  assert.match(text, /first@0: hello/)
  assert.match(text, /last@6: world/)
  assert.match(text, /omitted=0/)
})

test('a long line without a newline is a marked block, not a complete line', () => {
  const { create, state, output, time, followups } = setup()
  create()
  const long = 'x'.repeat(20_000)
  state.bytes = Buffer.from(long)
  state.chunks = [{ at: 0, text: long }]
  output(long.length)
  time.advance(800)
  assert.equal(followups.length, 1)
  const text = followups[0].content[0].text
  assert.match(text, /long-line block/)
  assert.ok(Buffer.byteLength(text, 'utf8') <= 8192)
})

test('literal and regex filters select only matching lines', () => {
  for (const [filter, expected] of [
    [{ pattern: 'err', regex: false }, ['error 1', 'err 2']],
    [{ pattern: '^err \\d$', regex: true }, ['err 2']],
  ]) {
    const { create, state, output, time, followups } = setup()
    create({ filter })
    state.bytes = Buffer.from('ok\nerror 1\nerr 2\n')
    state.chunks = [{ at: 0, text: state.bytes.toString('utf8') }]
    output(state.bytes.length)
    time.advance(800)
    assert.equal(followups.length, 1)
    const text = followups[0].content[0].text
    assert.match(text, new RegExp(`matched=${expected.length}`))
    for (const line of expected) assert.ok(text.includes(line))
    assert.ok(!text.includes('ok\n'))
  }
})

test('a burst larger than the line cap is split into bounded windows', () => {
  const { create, state, output, time, followups } = setup({
    budgets: {
      maxLinesPerNotification: 5,
      mergeWindowMs: 100,
      minIntervalMs: 0,
      maxNotifications: 20,
    },
  })
  create()
  const text = Array.from({ length: 17 }, (_, index) => `line ${index}`).join('\n') + '\n'
  state.bytes = Buffer.from(text)
  state.chunks = [{ at: 0, text }]
  output(state.bytes.length)
  time.advance(0)
  assert.equal(followups.length, 3)
  time.advance(100)
  assert.equal(followups.length, 4)
  const counts = followups.map((message) =>
    Number(/matched=(\d+)/.exec(message.content[0].text)[1]),
  )
  assert.deepEqual(counts, [5, 5, 5, 2])
})

test('the merge window turns N lines into one first/last notification', () => {
  const { create, state, output, time, followups } = setup()
  create()
  state.bytes = Buffer.from('one\ntwo\nthree\nfour\nfive\n')
  state.chunks = [{ at: 0, text: state.bytes.toString('utf8') }]
  output(state.bytes.length)
  time.advance(799)
  assert.equal(followups.length, 0)
  time.advance(1)
  assert.equal(followups.length, 1)
  const text = followups[0].content[0].text
  assert.match(text, /matched=5/)
  assert.match(text, /first@0: one/)
  assert.match(text, /last@19: five/)
  assert.match(text, /omitted=3/)
})

test('notify uses followup and quiet uses inject with a monitor user source', () => {
  const notify = setup()
  notify.create()
  notify.state.bytes = Buffer.from('line\n')
  notify.state.chunks = [{ at: 0, text: 'line\n' }]
  notify.output(5)
  notify.time.advance(800)
  assert.equal(notify.followups.length, 1)
  assert.equal(notify.injects.length, 0)
  assert.equal(notify.followups[0].role, 'user')
  assert.deepEqual(notify.followups[0].source, { kind: 'monitor' })

  const quiet = setup()
  quiet.create({ mode: 'quiet' })
  quiet.state.bytes = Buffer.from('line\n')
  quiet.state.chunks = [{ at: 0, text: 'line\n' }]
  quiet.output(5)
  quiet.time.advance(800)
  assert.equal(quiet.followups.length, 0)
  assert.equal(quiet.injects.length, 1)
  assert.deepEqual(quiet.injects[0].source, { kind: 'monitor' })
})

test('inserted and claimed never become a proven receipt; durable user/message does', () => {
  const { create, state, output, time, list, insertion, durable, followups } = setup()
  create()
  state.bytes = Buffer.from('line\n')
  state.chunks = [{ at: 0, text: 'line\n' }]
  output(5)
  time.advance(800)
  const message = followups[0]
  assert.equal(list().deliveries[0].state, 'submitted')
  insertion(message, 'inserted')
  assert.equal(list().deliveries[0].state, 'inserted')
  insertion(message, 'claimed')
  assert.equal(list().deliveries[0].state, 'claimed')
  assert.notEqual(list().deliveries[0].state, 'accepted')
  durable(message)
  assert.equal(list().deliveries[0].state, 'accepted')
})

test('discarded is a failure and the timeout is an explicit unknown', () => {
  const discarded = setup()
  discarded.create()
  discarded.state.bytes = Buffer.from('line\n')
  discarded.state.chunks = [{ at: 0, text: 'line\n' }]
  discarded.output(5)
  discarded.time.advance(800)
  discarded.insertion(discarded.followups[0], 'discarded')
  assert.equal(discarded.list().deliveries[0].state, 'discarded')

  const timed = setup()
  timed.create()
  timed.state.bytes = Buffer.from('line\n')
  timed.state.chunks = [{ at: 0, text: 'line\n' }]
  timed.output(5)
  timed.time.advance(800)
  timed.time.advance(120_000)
  assert.equal(timed.list().deliveries[0].state, 'unknown')
})

test('budget exhaustion stops delivery and keeps a bounded held summary', () => {
  const { create, service, address, state, output, time, list, followups, injects } = setup({
    budgets: { maxNotifications: 1, mergeWindowMs: 100, minIntervalMs: 0 },
  })
  create()
  state.bytes = Buffer.from('one\n')
  state.chunks = [{ at: 0, text: 'one\n' }]
  output(4)
  time.advance(100)
  assert.equal(followups.length, 1)
  assert.equal(list().exhausted, true)
  assert.equal(list().listening, false)

  state.bytes = Buffer.from('one\ntwo\nthree\n')
  state.chunks = [{ at: 0, text: 'one\ntwo\nthree\n' }]
  output(state.bytes.length)
  time.advance(1000)
  assert.equal(followups.length, 1)
  assert.equal(injects.length, 0)
  assert.equal(list().heldMatches, 2)

  // Explicit reset restores delivery; the held summary is delivered.
  const reset = service.resetBudget({
    address,
    jobId: JOB,
    budgets: { maxNotifications: 5, mergeWindowMs: 100, minIntervalMs: 0 },
  })
  assert.equal(reset.monitor.used.notifications, 0)
  time.advance(100)
  assert.equal(followups.length, 2)
})

test('settled performs one final read and produces no terminal model notice', () => {
  const { create, state, output, settled, time, list, followups } = setup()
  create()
  state.bytes = Buffer.from('before\n')
  state.chunks = [{ at: 0, text: 'before\n' }]
  output(7)
  time.advance(800)
  assert.equal(followups.length, 1)
  // Final bytes arrive before settlement; no post-settlement output frame.
  state.bytes = Buffer.from('before\nafter\n')
  state.chunks = [{ at: 0, text: 'before\nafter\n' }]
  settled('completed', 'exit code: 0')
  time.advance(800)
  assert.equal(followups.length, 2)
  assert.match(followups[1].content[0].text, /after/)
  assert.equal(list().stream, 'stream-ended')
  assert.equal(list().listening, false)
  // The official completion notice is not S10's job; no extra terminal message
  // beyond the line notifications.
  assert.equal(followups.length, 2)
})

test('killed and failed settlements are UI-only terminal facts', () => {
  for (const [status, stream, detail] of [
    ['killed', 'stopped', 'cancelled by the user'],
    ['failed', 'failed', 'exit code: 7'],
  ]) {
    const { create, state, output, settled, time, list, followups } = setup()
    create()
    state.bytes = Buffer.from('line\n')
    state.chunks = [{ at: 0, text: 'line\n' }]
    output(5)
    time.advance(800)
    assert.equal(followups.length, 1)
    settled(status, detail)
    time.advance(800)
    assert.equal(list().stream, stream)
    assert.equal(list().streamDetail, detail)
    // No S10 terminal model notice: only the line notification above.
    assert.equal(followups.length, 1)
  }
})

test('a lossy read is visible and never splices the carry', () => {
  const { create, state, output, time, list, followups } = setup()
  create()
  state.bytes = Buffer.from('partial')
  state.chunks = [{ at: 0, text: 'partial' }]
  output(7)
  state.lossy = true
  state.bytes = Buffer.from('partial-after\n')
  state.chunks = [{ at: 7, text: '-after\n' }]
  output(state.bytes.length)
  time.advance(800)
  assert.equal(list().gaps, 1)
  assert.equal(list().lossy, true)
  assert.equal(followups.length, 1)
  assert.match(followups[0].content[0].text, /-after/)
  assert.ok(!followups[0].content[0].text.includes('partial-after'))
})

test('a duplicate active create returns the existing monitor', () => {
  const { create, followups, state, output, time } = setup()
  const first = create()
  const second = create({ mode: 'quiet', from: 'earliest' })
  assert.equal(second.existing, true)
  assert.equal(second.monitor.mode, 'notify')
  assert.equal(first.monitor.id, second.monitor.id)
  // No second instance, so one line produces one delivery.
  state.bytes = Buffer.from('x\n')
  state.chunks = [{ at: 0, text: 'x\n' }]
  output(2)
  time.advance(800)
  assert.equal(followups.length, 1)
})

test('stopping a monitor never stops the observed job', () => {
  const { create, service, address, state } = setup()
  create()
  const result = service.stop({ address, jobId: JOB })
  assert.equal(result.monitor.listening, false)
  assert.deepEqual(state.kills, [])
})

test('an unknown job is not-found and a foreign address is refused', () => {
  const { service, address } = setup()
  assert.throws(
    () => service.create({ address, jobId: 'bash-missing', from: 'new', mode: 'notify' }),
    (error) => error.rpcName === 'nano/jobs/not-found',
  )
  assert.throws(
    () => service.list({ address: { rootSessionId: 'other' } }),
    (error) => error.rpcName === 'nano/conversation/address',
  )
})

test('invalid filters and budgets are explicit errors', () => {
  const { service, address } = setup()
  assert.throws(
    () => service.create({ address, jobId: JOB, from: 'new', mode: 'notify', filter: { pattern: '(', regex: true } }),
    (error) => error.rpcName === 'nano/monitor/filter-invalid',
  )
  assert.throws(
    () => service.create({ address, jobId: JOB, from: 'new', mode: 'notify', budgets: { maxNotifications: 0 } }),
    (error) => error.rpcName === 'nano/monitor/budget-invalid',
  )
})

test('one notification is bounded by the byte and token caps', () => {
  const monitor = {
    jobId: JOB,
    budgets: { maxNotificationBytes: 512, maxNotificationTokens: 128 },
  }
  const pending = {
    first: { text: 'x'.repeat(10_000), offset: 0, partial: false },
    last: { text: 'y'.repeat(10_000), offset: 10_000, partial: false },
    count: 2,
  }
  const text = buildNotification(monitor, pending)
  assert.ok(Buffer.byteLength(text, 'utf8') <= 512)
  assert.ok(Math.ceil(Buffer.byteLength(text, 'utf8') / 4) <= 128)
  assert.match(text, /truncated/)
})
