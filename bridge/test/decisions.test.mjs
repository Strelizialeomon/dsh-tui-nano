// Decision host unit tests. The fake context mirrors only the public surfaces
// S03 is allowed to use: the two answerer waterfalls and the userQuestions
// service methods that the official package exports.
import test from 'node:test'
import assert from 'node:assert/strict'

import { DecisionService } from '../decisions/service.mjs'

const ROOT = 'session-root'
const address = { rootSessionId: ROOT }

function fakeContext({ userQuestions, projections } = {}) {
  const handlers = new Map()
  const services = { userQuestions, sessionProjections: projections }
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
    get(key) {
      return services[key]
    },
    emit(event, payload) {
      for (const handler of [...(handlers.get(event) ?? [])]) handler(payload)
    },
  }
}

function fakeSession(events = []) {
  return {
    id: ROOT,
    snapshotEvents() {
      return events
    },
  }
}

function fakeUserQuestions({ answer } = {}) {
  return {
    claimed: [],
    answered: [],
    async *attachWait(_agent, callId, signal) {
      this.claimed.push(callId)
      yield { remainingMs: 60_000 }
      await new Promise((resolve) => {
        signal.addEventListener('abort', resolve, { once: true })
      })
    },
    answer(agent, callId, payload) {
      this.answered.push({ agent, callId, payload })
      if (answer !== undefined) return answer(callId, payload)
      return true
    },
  }
}

function serviceFor({ events = [], userQuestions, projections } = {}) {
  const ctx = fakeContext({ userQuestions, projections })
  const agent = { id: ROOT }
  const session = fakeSession(events)
  const published = []
  const hub = { publish: (kind, payload) => published.push({ kind, payload }) }
  const service = new DecisionService({ ctx, hub, agent, session, rootSessionId: ROOT })
  return { ctx, agent, session, published, service }
}

function pendingOf(published, kind) {
  return published.find((entry) => entry.kind === kind && entry.payload.state === 'pending')
}

test('an approval publishes its linked command and settles with the official outcome', async () => {
  const events = [
    {
      type: 'tool/call',
      data: { callId: 'c-1', name: 'bash', arguments: '{"command":"rm -rf build"}' },
    },
  ]
  const { published, service } = serviceFor({ events })
  const controller = new AbortController()
  const answer = service.onApproval(
    {
      agent: { id: ROOT },
      toolName: 'bash',
      reason: 'sandbox escalation',
      callId: 'c-1',
      signal: controller.signal,
    },
    () => Promise.resolve('unavailable'),
  )
  const pending = pendingOf(published, 'decisions.approval')
  assert.equal(pending.payload.command, 'rm -rf build')
  assert.equal(pending.payload.toolName, 'bash')
  const receipt = service.answerApproval({
    address,
    requestId: pending.payload.requestId,
    outcome: 'allowed-once',
  })
  assert.deepEqual(receipt, { accepted: true })
  assert.equal(await answer, 'allowed-once')
  const settled = published.find(
    (entry) => entry.kind === 'decisions.approval' && entry.payload.state === 'settled',
  )
  assert.equal(settled.payload.outcome, 'allowed-once')
})

test('an approval without a callId never shows a guessed command', async () => {
  const { published, service } = serviceFor()
  const answer = service.onApproval(
    { agent: { id: ROOT }, toolName: 'write', reason: 'write outside the sandbox' },
    () => Promise.resolve('unavailable'),
  )
  const pending = pendingOf(published, 'decisions.approval')
  assert.equal('command' in pending.payload, false)
  assert.equal('callId' in pending.payload, false)
  service.answerApproval({
    address,
    requestId: pending.payload.requestId,
    outcome: 'rejected',
  })
  assert.equal(await answer, 'rejected')
})

test('an aborted approval is withdrawn, not auto-answered', async () => {
  const { published, service } = serviceFor()
  const controller = new AbortController()
  const answer = service.onApproval(
    { agent: { id: ROOT }, toolName: 'bash', signal: controller.signal },
    () => Promise.resolve('unavailable'),
  )
  const pending = pendingOf(published, 'decisions.approval')
  controller.abort()
  await new Promise((resolve) => setImmediate(resolve))
  const withdrawn = published.find(
    (entry) => entry.kind === 'decisions.approval' && entry.payload.state === 'withdrawn',
  )
  assert.equal(withdrawn.payload.requestId, pending.payload.requestId)
  assert.equal(service.pendingApprovals.size, 0)
  assert.throws(
    () =>
      service.answerApproval({
        address,
        requestId: pending.payload.requestId,
        outcome: 'allowed-once',
      }),
    (error) => error.code === 1019,
  )
  // The answerer promise is intentionally left unsettled; the official
  // waterfall settles from the cancelled turn, and exit never answers for it.
  void answer
})

test('approval requests for another agent delegate to the next answerer', async () => {
  const { published, service } = serviceFor()
  const answer = await service.onApproval(
    { agent: { id: 'other' }, toolName: 'bash' },
    () => Promise.resolve('unavailable'),
  )
  assert.equal(answer, 'unavailable')
  assert.equal(published.length, 0)
})

test('a live question resolves with the exact answer batch', async () => {
  const { published, service } = serviceFor()
  const answers = [
    { id: 'q1', selected: ['A'] },
    { id: 'q2', selected: [], custom: 'other' },
  ]
  const answer = service.onQuestion(
    {
      agent: { id: ROOT },
      questions: [
        { id: 'q1', question: 'Pick', options: [{ label: 'A' }] },
        { id: 'q2', question: 'Other', options: [{ label: 'B' }], multiSelect: true },
      ],
    },
    () => Promise.resolve({ answers: [] }),
  )
  const pending = pendingOf(published, 'decisions.question')
  service.answerQuestion({
    address,
    requestId: pending.payload.requestId,
    answers,
  })
  assert.deepEqual(await answer, { answers })
  const settled = published.find(
    (entry) => entry.kind === 'decisions.question' && entry.payload.state === 'settled',
  )
  assert.deepEqual(settled.payload.answers, answers)
})

test('shutdown never answers a pending request', async () => {
  const userQuestions = fakeUserQuestions()
  const { published, service } = serviceFor({ userQuestions })
  const controller = new AbortController()
  const approval = service.onApproval(
    { agent: { id: ROOT }, toolName: 'bash', signal: controller.signal },
    () => Promise.resolve('unavailable'),
  )
  const question = service.onQuestion(
    {
      agent: { id: ROOT },
      questions: [{ id: 'q1', question: 'Pick', options: [{ label: 'A' }] }],
      signal: new AbortController().signal,
    },
    () => Promise.resolve({ answers: [] }),
  )
  service.close()
  assert.equal(service.pendingApprovals.size, 0)
  assert.equal(service.pendingQuestions.size, 0)
  assert.equal(
    published.some((entry) => entry.payload.state === 'settled'),
    false,
  )
  void approval
  void question
})

test('a discarded queued reply makes the continued question answerable again', async () => {
  const { ctx, published, service } = serviceFor()
  service.onQuestion(
    {
      agent: { id: ROOT },
      questions: [{ id: 'q1', question: 'Pick', options: [{ label: 'A' }] }],
      wait: { callId: 'c-9' },
      signal: new AbortController().signal,
    },
    () => Promise.resolve({ answers: [] }),
  )
  ctx.emit('agent/inbox/discarded', {
    agent: { id: ROOT },
    message: { source: { kind: 'user-question-reply', callId: 'c-9' } },
  })
  const continued = published.find(
    (entry) =>
      entry.kind === 'decisions.question' &&
      entry.payload.state === 'continued' &&
      entry.payload.callId === 'c-9',
  )
  assert.ok(continued !== undefined)
})

test('a timed question claims the wait and publishes the remaining duration', async () => {
  const userQuestions = fakeUserQuestions()
  const { published, service } = serviceFor({ userQuestions })
  const answer = service.onQuestion(
    {
      agent: { id: ROOT },
      questions: [{ id: 'q1', question: 'Pick', options: [{ label: 'A' }] }],
      wait: { callId: 'c-1', timed: true },
      signal: new AbortController().signal,
    },
    () => Promise.resolve({ answers: [] }),
  )
  await new Promise((resolve) => setImmediate(resolve))
  assert.deepEqual(userQuestions.claimed, ['c-1'])
  const timed = published.find(
    (entry) => entry.kind === 'decisions.question' && entry.payload.remainingMs === 60_000,
  )
  assert.ok(timed !== undefined)
  service.answerQuestion({
    address,
    requestId: timed.payload.requestId,
    answers: [{ id: 'q1', selected: ['A'] }],
  })
  assert.deepEqual(await answer, { answers: [{ id: 'q1', selected: ['A'] }] })
})

test('a local countdown expiry rejects with ASK_TIMED_OUT and leaves the question continued', async () => {
  const userQuestions = fakeUserQuestions()
  const { published, service } = serviceFor({ userQuestions })
  const answer = service.onQuestion(
    {
      agent: { id: ROOT },
      questions: [{ id: 'q1', question: 'Pick', options: [{ label: 'A' }] }],
      wait: { callId: 'c-1', timed: true },
      signal: new AbortController().signal,
    },
    () => Promise.resolve({ answers: [] }),
  )
  await new Promise((resolve) => setImmediate(resolve))
  const pending = pendingOf(published, 'decisions.question')
  const receipt = service.answerQuestion({
    address,
    requestId: pending.payload.requestId,
    expire: true,
  })
  assert.deepEqual(receipt, { accepted: true, mode: 'live' })
  const error = await answer.catch((reason) => reason)
  assert.equal(error.name, 'UserQuestionError')
  assert.equal(error.code, 'ASK_TIMED_OUT')
  const expired = published.find(
    (entry) => entry.kind === 'decisions.question' && entry.payload.state === 'expired',
  )
  assert.equal(expired.payload.callId, 'c-1')
})

test('a continued answer goes through the official answer method', async () => {
  const userQuestions = fakeUserQuestions()
  const { published, service } = serviceFor({ userQuestions })
  const receipt = service.answerQuestion({
    address,
    callId: 'c-9',
    answers: [{ id: 'q1', selected: ['A'] }],
  })
  assert.deepEqual(receipt, { accepted: true, mode: 'continued' })
  assert.equal(userQuestions.answered.length, 1)
  assert.equal(userQuestions.answered[0].callId, 'c-9')
  const submitted = published.find(
    (entry) => entry.kind === 'decisions.question' && entry.payload.state === 'submitted',
  )
  assert.equal(submitted.payload.callId, 'c-9')
})

test('a closed continued question returns not-continued instead of claiming success', () => {
  const userQuestions = fakeUserQuestions({ answer: () => false })
  const { service } = serviceFor({ userQuestions })
  const receipt = service.answerQuestion({
    address,
    callId: 'c-9',
    answers: [{ id: 'q1', selected: ['A'] }],
  })
  assert.deepEqual(receipt, {
    accepted: false,
    mode: 'continued',
    reason: 'not-continued',
  })
})

test('REPLY_QUEUED keeps its distinct error code', () => {
  const userQuestions = fakeUserQuestions({
    answer: () => {
      const error = new Error('a reply is already queued')
      error.code = 'REPLY_QUEUED'
      throw error
    },
  })
  const { service } = serviceFor({ userQuestions })
  assert.throws(
    () =>
      service.answerQuestion({
        address,
        callId: 'c-9',
        answers: [{ id: 'q1', selected: ['A'] }],
      }),
    (error) => error.code === 1021,
  )
})

test('an unknown live request and an invalid batch are distinguishable', () => {
  const { service } = serviceFor()
  assert.throws(
    () =>
      service.answerQuestion({
        address,
        requestId: 'missing',
        answers: [{ id: 'q1', selected: ['A'] }],
      }),
    (error) => error.code === 1019,
  )
  assert.throws(
    () =>
      service.answerQuestion({
        address,
        requestId: 'missing',
        answers: [{ id: 'q1', selected: [1] }],
      }),
    (error) => error.code === 1020,
  )
})

test('a snapshot restores pending cards and the projection after reconnect', async () => {
  const projection = {
    active: [{ callId: 'c-1', questions: [{ id: 'q', question: 'Q' }], state: 'continued' }],
    settled: [],
  }
  const projections = {
    stateOf: () => ({ questions: projection }),
    onChanged: () => () => {},
  }
  const { published, service } = serviceFor({ projections })
  const controller = new AbortController()
  service.onApproval(
    { agent: { id: ROOT }, toolName: 'bash', signal: controller.signal },
    () => Promise.resolve('unavailable'),
  )
  assert.ok(pendingOf(published, 'decisions.approval') !== undefined)
  const snapshot = service.snapshot({ address })
  assert.equal(snapshot.approvals.length, 1)
  assert.equal(snapshot.questions.length, 0)
  assert.deepEqual(snapshot.projection, projection)
})

test('address validation refuses another root session', () => {
  const { service } = serviceFor()
  assert.throws(
    () => service.snapshot({ address: { rootSessionId: 'other' } }),
    (error) => error.code === 1008,
  )
  assert.throws(
    () => service.snapshot({ address: { rootSessionId: ROOT, childSessionId: 'child' } }),
    (error) => error.code === 1008,
  )
})
