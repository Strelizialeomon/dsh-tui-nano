// S09 bridge service tests. The official controller is faked at its public
// method boundary; framing, sequencing and live-only notifications are real.
import test from 'node:test'
import assert from 'node:assert/strict'

import { AttachmentInput, TerminalService } from '../terminal/service.mjs'

const SESSION = 'session-fixture'
const INFO = {
  id: 'term-1',
  title: 'zsh',
  shell: { path: '/bin/zsh', args: ['-i'], name: 'zsh' },
  cwd: '/work',
  cols: 80,
  rows: 24,
  state: 'running',
  exitCode: null,
}

class FakeWriter {
  constructor() {
    this.frames = []
    this.waiters = []
  }

  async sendControl(buffer) {
    const text = buffer.toString('utf8').trim()
    const frame = JSON.parse(text)
    this.frames.push(frame)
    for (const waiter of this.waiters.splice(0)) waiter()
  }

  async waitFor(predicate, label) {
    const deadline = Date.now() + 2000
    while (Date.now() < deadline) {
      if (this.frames.some(predicate)) return
      await new Promise((resolve) => {
        this.waiters.push(resolve)
        setTimeout(resolve, 10)
      })
    }
    assert.fail(`timed out waiting for ${label}; got ${JSON.stringify(this.frames)}`)
  }
}

class FakeController {
  constructor({ createError, inputMode = 'consume', outputError, badState, closeFailures = 0 } = {}) {
    this.createError = createError
    this.inputMode = inputMode
    this.outputError = outputError
    this.badState = badState
    this.closeFailures = closeFailures
    this.closeCalls = 0
    this.infos = new Map()
    this.ctx = { extend: (meta) => ({ ...meta }) }
  }

  environment() {
    return {
      cwd: '/work',
      maxInputBytes: 1024,
      maxBufferedBytes: 4096,
      maxCols: 120,
      maxRows: 40,
      scrollback: 100,
    }
  }

  shells() {
    return [{ path: '/bin/zsh', args: ['-i'], name: 'zsh' }]
  }

  list() {
    return [...this.infos.values()]
  }

  async create(agent, request) {
    if (this.createError !== undefined) throw this.createError
    const info = {
      ...INFO,
      id: request.id,
      cols: request.cols,
      rows: request.rows,
      state: this.badState ?? INFO.state,
    }
    this.infos.set(request.id, info)
    return info
  }

  retain(sessionId, id, signal) {
    const controller = this
    return (async function* () {
      yield { type: 'retained' }
      while (!signal.aborted) await new Promise((resolve) => setTimeout(resolve, 10))
      void controller
    })()
  }

  attach() {
    const controller = this
    return (async function* () {
      const info = controller.infos.values().next().value ?? INFO
      yield { type: 'snapshot', sequence: 0, screen: '\u001b[31mready', info, writable: true }
      if (controller.outputError !== undefined) throw controller.outputError
      if (controller.inputMode === 'consume') {
        const input = controller.ctx.invocation.uplink()
        for await (const item of input) {
          yield { type: 'ack', sequence: item.sequence }
          break
        }
      }
      yield { type: 'output', sequence: 1, data: 'hello\r\n' }
      const exited = { ...info, state: 'exited', exitCode: 0 }
      yield { type: 'state', info: exited, writable: false }
    })()
  }

  rename(agent, id, title) {
    const info = this.infos.get(id)
    if (info !== undefined) this.infos.set(id, { ...info, title })
  }

  async close(agent, id) {
    this.closeCalls += 1
    if (this.closeFailures > 0) {
      this.closeFailures -= 1
      throw new Error('terminal cleanup failed; surviving pids: 1')
    }
    this.infos.delete(id)
  }

  ctxForAttach() {
    return this.ctx
  }
}

function serviceWith(controller, writer = new FakeWriter()) {
  const service = new TerminalService({
    ctx: { get: (key) => (key === 'terminalController' ? controller : undefined) },
    writer,
    agent: { id: SESSION },
    session: { id: SESSION },
    rootSessionId: SESSION,
    nonce: 'n',
    nonceRequired: false,
  })
  return { service, writer }
}

test('missing controller reports unsupported capability through errors', () => {
  const { service } = serviceWith(undefined)
  assert.equal(service.available, false)
  assert.throws(() => service.environment(), (error) => error.code === 1019)
})

test('create is idempotent for the same open identity', async () => {
  const controller = new FakeController()
  const { service } = serviceWith(controller)
  const first = await service.create({ terminalId: 'term-1', cols: 80, rows: 24 })
  const again = await service.create({ terminalId: 'term-1', cols: 80, rows: 24 })
  assert.deepEqual(first, again)
  assert.equal(service.list().terminals.length, 1)
})

test('terminal limit errors map to the Nano error table', async () => {
  const error = Object.assign(new Error('limit'), { code: 'terminal/limit-reached', details: { limit: 8 } })
  const { service } = serviceWith(new FakeController({ createError: error }))
  await assert.rejects(
    service.create({ terminalId: 'term-1', cols: 80, rows: 24 }),
    (rejected) => rejected.code === 1020 && rejected.data.limit === 8,
  )
})

test('attach returns the snapshot and forwards live frames', async () => {
  const controller = new FakeController({ inputMode: 'none' })
  const { service, writer } = serviceWith(controller)
  await service.create({ terminalId: 'term-1', cols: 80, rows: 24 })
  const baseline = await service.attach({ sessionId: SESSION, terminalId: 'term-1' })
  assert.equal(baseline.sequence, 0)
  assert.equal(baseline.snapshot, '\u001b[31mready')
  assert.equal(baseline.writable, true)
  await writer.waitFor((frame) => frame.method === 'nano.terminal.output', 'output')
  await writer.waitFor((frame) => frame.method === 'nano.terminal.state', 'state')
  await writer.waitFor((frame) => frame.method === 'nano.terminal.ended', 'ended')
  const ended = writer.frames.find((frame) => frame.method === 'nano.terminal.ended')
  assert.equal(ended.params.reason, 'drained')
  const state = writer.frames.find((frame) => frame.method === 'nano.terminal.state')
  assert.equal(state.params.writable, false)
  assert.equal(state.params.info.state, 'exited')
})

test('input and resize share one attachment-local sequence', async () => {
  const controller = new FakeController({ inputMode: 'consume' })
  const { service, writer } = serviceWith(controller)
  await service.create({ terminalId: 'term-1', cols: 80, rows: 24 })
  const baseline = await service.attach({ sessionId: SESSION, terminalId: 'term-1' })
  service.handleNotification('nano.terminal.input', {
    attachmentId: baseline.attachmentId,
    sequence: 1,
    bytes: 'ls\r',
  })
  await writer.waitFor((frame) => frame.method === 'nano.terminal.ack', 'ack')
  const ack = writer.frames.find((frame) => frame.method === 'nano.terminal.ack')
  assert.equal(ack.params.sequence, 1)
})

test('a sequence gap ends the attachment without killing the terminal', async () => {
  const controller = new FakeController({ inputMode: 'none' })
  const { service, writer } = serviceWith(controller)
  await service.create({ terminalId: 'term-1', cols: 80, rows: 24 })
  const baseline = await service.attach({ sessionId: SESSION, terminalId: 'term-1' })
  service.handleNotification('nano.terminal.input', {
    attachmentId: baseline.attachmentId,
    sequence: 2,
    bytes: 'x',
  })
  await writer.waitFor((frame) => frame.method === 'nano.terminal.ended', 'ended')
  const ended = writer.frames.find((frame) => frame.method === 'nano.terminal.ended')
  assert.equal(ended.params.reason, 'input-failed')
  assert.match(ended.params.detail, /sequence gap/u)
  assert.equal(service.list().terminals.length, 1)
})

test('retain holds the process and release releases the hold', async () => {
  const controller = new FakeController()
  const { service } = serviceWith(controller)
  await service.create({ terminalId: 'term-1', cols: 80, rows: 24 })
  const { holdId } = await service.retain({ terminalId: 'term-1' })
  assert.equal(service.holds.size, 1)
  await service.close({ terminalId: 'term-1' })
  assert.equal(service.holds.size, 0)
  assert.equal(holdId.startsWith('hold-'), true)
})

test('close waits for official cleanup and removes the identity', async () => {
  const controller = new FakeController({ inputMode: 'none' })
  const { service } = serviceWith(controller)
  await service.create({ terminalId: 'term-1', cols: 80, rows: 24 })
  await service.attach({ sessionId: SESSION, terminalId: 'term-1' })
  const result = await service.close({ terminalId: 'term-1' })
  assert.deepEqual(result, { closed: true })
  assert.equal(service.list().terminals.length, 0)
})

test('attachment input queue fails with the provider error', async () => {
  const input = new AttachmentInput()
  const iterator = input[Symbol.asyncIterator]()
  input.fail(new Error('inputFull'))
  await assert.rejects(iterator.next(), /inputFull/u)
})

test('the terminal surface never registers an Agent tool or Monitor route', async () => {
  const controller = new FakeController({ inputMode: 'none' })
  const registered = []
  const tools = {
    register(name) {
      registered.push(name)
    },
  }
  const service = new TerminalService({
    ctx: {
      get: (key) => {
        if (key === 'terminalController') return controller
        if (key === 'tools') return tools
        return undefined
      },
      on: () => undefined,
    },
    writer: new FakeWriter(),
    agent: { id: SESSION },
    session: { id: SESSION },
    rootSessionId: SESSION,
    nonce: 'n',
    nonceRequired: false,
  })
  await service.create({ terminalId: 'term-1', cols: 80, rows: 24 })
  await service.attach({ sessionId: SESSION, terminalId: 'term-1' })
  assert.deepEqual(registered, [])
  assert.equal(service.available, true)
})

test('attachment input return unblocks a pending next', async () => {
  const input = new AttachmentInput()
  const iterator = input[Symbol.asyncIterator]()
  const pending = iterator.next()
  await iterator.return()
  assert.deepEqual(await pending, { value: undefined, done: true })
})

test('an out-of-bounds resize ends the attachment but keeps the terminal', async () => {
  const controller = new FakeController({ inputMode: 'none' })
  const { service, writer } = serviceWith(controller)
  await service.create({ terminalId: 'term-1', cols: 80, rows: 24 })
  const baseline = await service.attach({ sessionId: SESSION, terminalId: 'term-1' })
  service.handleNotification('nano.terminal.resize', {
    attachmentId: baseline.attachmentId,
    sequence: 1,
    cols: 1000,
    rows: 24,
  })
  await writer.waitFor((frame) => frame.method === 'nano.terminal.ended', 'ended')
  const ended = writer.frames.find((frame) => frame.method === 'nano.terminal.ended')
  assert.equal(ended.params.reason, 'input-failed')
  assert.match(ended.params.detail, /resize is outside/u)
  assert.equal(service.list().terminals.length, 1)
})

test('attach requires the host session id and can infer a single terminal', async () => {
  const controller = new FakeController({ inputMode: 'none' })
  const { service } = serviceWith(controller)
  await service.create({ terminalId: 'term-1', cols: 80, rows: 24 })
  await assert.rejects(
    service.attach({ sessionId: 'other-session', terminalId: 'term-1' }),
    (error) => error.code === -32602,
  )
  const baseline = await service.attach({ sessionId: SESSION })
  assert.equal(baseline.terminalId, 'term-1')
  assert.equal(typeof baseline.snapshot, 'string')
})

test('an unknown terminal state is rejected instead of being treated as running', async () => {
  const controller = new FakeController({ badState: 'future' })
  const { service } = serviceWith(controller)
  await assert.rejects(
    service.create({ terminalId: 'term-1', cols: 80, rows: 24 }),
    (error) => error.code === 1026 && /unknown terminal state/u.test(error.message),
  )
})

test('a follower buffer overflow ends as slow-consumer', async () => {
  const controller = new FakeController({
    inputMode: 'none',
    outputError: new Error('Terminal output consumer exceeded its buffer'),
  })
  const { service, writer } = serviceWith(controller)
  await service.create({ terminalId: 'term-1', cols: 80, rows: 24 })
  await service.attach({ sessionId: SESSION, terminalId: 'term-1' })
  await writer.waitFor((frame) => frame.method === 'nano.terminal.ended', 'ended')
  const ended = writer.frames.find((frame) => frame.method === 'nano.terminal.ended')
  assert.equal(ended.params.reason, 'slow-consumer')
})

test('rename reaches the official controller', async () => {
  const controller = new FakeController()
  const { service } = serviceWith(controller)
  await service.create({ terminalId: 'term-1', cols: 80, rows: 24 })
  assert.deepEqual(service.rename({ terminalId: 'term-1', title: 'work' }), { renamed: true })
  assert.equal(controller.infos.get('term-1').title, 'work')
})

test('input queue clears queued commands on failure and enforces the byte cap', () => {
  const input = new AttachmentInput(4)
  assert.equal(input.push({ type: 'write', sequence: 1, data: 'abc' }), true)
  assert.equal(input.push({ type: 'write', sequence: 2, data: 'de' }), false)
  assert.equal(input.items.length, 0)
  assert.equal(input.closed, true)
  const bounded = new AttachmentInput(4)
  assert.equal(bounded.push({ type: 'write', sequence: 1, data: 'abcd' }), true)
  assert.equal(bounded.push({ type: 'resize', sequence: 2, cols: 80, rows: 24 }), false)
})

test('acknowledge releases input byte budget', () => {
  const input = new AttachmentInput(4)
  assert.equal(input.push({ type: 'write', sequence: 1, data: 'abcd' }), true)
  assert.equal(input.push({ type: 'write', sequence: 2, data: 'efgh' }), false)
  const accepted = new AttachmentInput(4)
  assert.equal(accepted.push({ type: 'write', sequence: 1, data: 'abcd' }), true)
  accepted.acknowledge(1)
  assert.equal(accepted.push({ type: 'write', sequence: 2, data: 'efgh' }), true)
})

test('closeAll retries a transient provider cleanup failure', async () => {
  const controller = new FakeController({ closeFailures: 2 })
  const { service } = serviceWith(controller)
  await service.create({ terminalId: 'term-1', cols: 80, rows: 24 })
  await service.closeAll()
  assert.equal(controller.closeCalls, 3)
})
