// Contract conformance for the TypeScript side. The JSON contract is the
// source of truth; this test fails when the bridge's declared method set,
// limits, error codes, lifecycle edges, or fixtures drift from it.
import test from 'node:test'
import assert from 'node:assert/strict'

import {
  CONTRACT,
  ERROR_CODES,
  EVENT_KINDS,
  IMPLEMENTED_METHODS as CONTRACT_IMPLEMENTED,
  LIFECYCLE_EDGES,
  LIMITS,
  RESERVED_METHODS,
} from '../../protocol/index.mjs'
import {
  IMPLEMENTED_METHODS,
  IMPLEMENTED_NOTIFICATION_METHODS,
  IMPLEMENTED_REQUEST_METHODS,
} from '../runtime/methods.mjs'
import {
  encodeFrame,
  parseFrameLine,
  validateEvent,
  validateHello,
  validateStartupError,
  validateStreamEnded,
} from '../runtime/frames.mjs'

test('implemented methods match the contract', () => {
  assert.deepEqual([...IMPLEMENTED_METHODS].sort(), [...CONTRACT_IMPLEMENTED.keys()].sort())
  for (const [name, method] of CONTRACT_IMPLEMENTED) {
    assert.equal(method.owner, 'S01', `${name} owner`)
    if (method.kind === 'request') {
      assert.ok(IMPLEMENTED_REQUEST_METHODS.includes(name), `${name} must be dispatched as a request`)
    } else {
      assert.ok(IMPLEMENTED_NOTIFICATION_METHODS.includes(name), `${name} must be emitted as a notification`)
    }
  }
})

test('reserved methods have non-S01 owners', () => {
  for (const [name, method] of RESERVED_METHODS) {
    assert.notEqual(method.owner, 'S01', `${name} is reserved but owned by S01`)
    assert.ok(['S03', 'S06', 'S09'].includes(method.owner), `${name} owner`)
  }
})

test('limits and error codes match the contract', () => {
  assert.equal(LIMITS.maxFrameBytes, 33554432)
  assert.equal(LIMITS.maxPendingRequests, 64)
  assert.equal(LIMITS.maxAttachmentChunkBytes, 49152)
  assert.equal(LIMITS.gracefulShutdownMs, 10000)
  assert.equal(LIMITS.termGraceMs, 3000)
  assert.equal(LIMITS.mountLockStaleMs, 30000)
  assert.equal(ERROR_CODES.get('nano/generation-mismatch'), 1005)
  assert.equal(ERROR_CODES.get('nano/unsupported'), 1008)
  assert.equal(ERROR_CODES.get('nano/closed'), 1010)
})

test('lifecycle edges are exactly the contract table', () => {
  for (const [from, to] of CONTRACT.lifecycle.transitions) {
    assert.ok(LIFECYCLE_EDGES.has(`${from}->${to}`))
  }
  assert.equal(LIFECYCLE_EDGES.size, CONTRACT.lifecycle.transitions.length)
})

test('event kinds match', () => {
  assert.deepEqual([...EVENT_KINDS.keys()], ['session.event'])
})

test('request fixture parses as a request', () => {
  const frame = parseFrameLine(JSON.stringify(CONTRACT.fixtures.request))
  assert.equal(frame.type, 'request')
  assert.equal(frame.method, 'nano.subscribe')
  assert.equal(frame.params.address.rootSessionId, 'session-fixture')
})

test('response and error fixtures parse', () => {
  const response = parseFrameLine(JSON.stringify(CONTRACT.fixtures.response))
  assert.equal(response.type, 'response')
  assert.equal(response.id, 'req-1')
  assert.ok(response.result)
  const error = parseFrameLine(JSON.stringify(CONTRACT.fixtures.errorResponse))
  assert.equal(error.type, 'response')
  assert.equal(error.error.code, 1001)
})

test('hello fixture validates and echoes contract limits', () => {
  const frame = parseFrameLine(JSON.stringify(CONTRACT.fixtures.hello))
  assert.equal(frame.type, 'notification')
  assert.equal(frame.method, 'nano.hello')
  const hello = validateHello(frame.params)
  assert.equal(hello.rootSessionId, 'session-fixture')
  assert.equal(hello.limits.maxFrameBytes, LIMITS.maxFrameBytes)
  assert.equal(hello.capabilities['tool-ask-user'].mode, 'blocking')
})

test('startupError, event, and streamEnded fixtures validate', () => {
  const startup = parseFrameLine(JSON.stringify(CONTRACT.fixtures.startupError))
  assert.equal(validateStartupError(startup.params).code, 'occupied')
  const event = parseFrameLine(JSON.stringify(CONTRACT.fixtures.event))
  assert.equal(validateEvent(event.params, { runtimeId: 'rt-fixture', generation: 1 }).kind, 'session.event')
  const ended = parseFrameLine(JSON.stringify(CONTRACT.fixtures.streamEnded))
  assert.equal(validateStreamEnded(ended.params, { runtimeId: 'rt-fixture', generation: 1 }).outcome, 'cancelled')
})

test('shutdown report fixture has the four facts', () => {
  const report = CONTRACT.fixtures.shutdownResult.result
  assert.equal(report.clean, true)
  const names = report.steps.map((step) => step.name)
  for (const name of ['accepting', 'subscriptions', 'agents', 'persistence', 'dispose', 'ledger']) {
    assert.ok(names.includes(name), `missing ${name}`)
  }
  assert.ok(report.steps.every((step) => step.status === 'stopped'))
})

test('encodeFrame produces one newline-terminated frame', () => {
  const bytes = encodeFrame(CONTRACT.fixtures.request)
  assert.equal(bytes.at(-1), 0x0a)
  assert.equal(bytes.toString('utf8').trim().split('\n').length, 1)
})
