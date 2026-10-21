// Byte-level and payload-level framing tests.
import test from 'node:test'
import assert from 'node:assert/strict'

import { CONTRACT } from '../../protocol/index.mjs'
import {
  FrameSplitter,
  encodeFrame,
  parseFrameLine,
  validateEvent,
  validateHello,
  validateStartupError,
  validateStreamEnded,
} from '../runtime/frames.mjs'
import { ProtocolViolation, RpcError } from '../runtime/errors.mjs'

const helloFrame = structuredClone(CONTRACT.fixtures.hello)
const hello = helloFrame.params

test('splits frames across arbitrary chunks', () => {
  const splitter = new FrameSplitter()
  assert.deepEqual(splitter.push(Buffer.from('{"a":1}\n{"b"')), ['{"a":1}'])
  assert.deepEqual(splitter.push(Buffer.from(':2}\n')), ['{"b":2}'])
  splitter.end()
})

test('handles a UTF-8 sequence split across chunks', () => {
  const splitter = new FrameSplitter()
  const line = Buffer.from('{"k":"中文"}\n', 'utf8')
  assert.deepEqual(splitter.push(line.subarray(0, 7)), [])
  assert.deepEqual(splitter.push(line.subarray(7)), ['{"k":"中文"}'])
})

test('rejects an oversized frame before a newline', () => {
  const splitter = new FrameSplitter({ maxFrameBytes: 4 })
  assert.throws(
    () => splitter.push(Buffer.from('abcdef')),
    (error) => error instanceof ProtocolViolation && error.code === 1003,
  )
})

test('rejects invalid UTF-8 and blank frames', () => {
  const splitter = new FrameSplitter()
  assert.throws(() => splitter.push(Buffer.from([0xff, 0x0a])), ProtocolViolation)
  assert.throws(() => new FrameSplitter().push(Buffer.from('\n')), ProtocolViolation)
})

test('rejects an unterminated stream', () => {
  const splitter = new FrameSplitter()
  splitter.push(Buffer.from('{"a":1}'))
  assert.throws(() => splitter.end(), ProtocolViolation)
})

test('parses request, notification, response, and error frames', () => {
  const request = parseFrameLine(JSON.stringify({ jsonrpc: '2.0', id: 'r1', method: 'nano.subscribe', params: {} }))
  assert.equal(request.type, 'request')
  const notification = parseFrameLine(JSON.stringify({ jsonrpc: '2.0', method: 'nano.hello', params: {} }))
  assert.equal(notification.type, 'notification')
  const response = parseFrameLine(JSON.stringify({ jsonrpc: '2.0', id: 'r1', result: {} }))
  assert.equal(response.type, 'response')
  const error = parseFrameLine(
    JSON.stringify({ jsonrpc: '2.0', id: 'r1', error: { code: 1001, message: 'not ready' } }),
  )
  assert.equal(error.error.code, 1001)
})

test('rejects malformed envelopes', () => {
  assert.throws(() => parseFrameLine('not json'), (error) => error.code === -32700)
  assert.throws(() => parseFrameLine(JSON.stringify({ jsonrpc: '1.0', id: 'r1', method: 'nano.x', params: {} })))
  assert.throws(() =>
    parseFrameLine(JSON.stringify({ jsonrpc: '2.0', id: 'r1', method: 'other.x', params: {} })),
  )
  assert.throws(() =>
    parseFrameLine(JSON.stringify({ jsonrpc: '2.0', id: 'r1', result: {}, error: { code: 1, message: 'x' } })),
  )
  assert.throws(() =>
    parseFrameLine(JSON.stringify({ jsonrpc: '2.0', id: 'r1', error: { code: 'x', message: 'x' } })),
  )
})

test('fallback transport requires the generation nonce', () => {
  const frame = { jsonrpc: '2.0', id: 'r1', method: 'nano.subscribe', params: {} }
  assert.throws(
    () => parseFrameLine(JSON.stringify(frame), { requireNonce: true, nonce: 'n1' }),
    /nonce/,
  )
  const withNonce = parseFrameLine(JSON.stringify({ ...frame, nonce: 'n1' }), { requireNonce: true, nonce: 'n1' })
  assert.equal(withNonce.type, 'request')
})

test('hello rejects a different major, bad limits, and unknown required capabilities', () => {
  const wrongMajor = structuredClone(hello)
  wrongMajor.protocol.major = 2
  assert.throws(() => validateHello(wrongMajor), /major/)

  const wrongLimits = structuredClone(hello)
  wrongLimits.limits.maxFrameBytes = 1
  assert.throws(() => validateHello(wrongLimits), /limits/)

  const unknownRequired = structuredClone(hello)
  unknownRequired.capabilities['future-required'] = { status: 'supported', required: true }
  assert.throws(() => validateHello(unknownRequired), /unknown required capability/)

  const unknownOptional = structuredClone(hello)
  unknownOptional.capabilities['future-optional'] = { status: 'supported' }
  assert.doesNotThrow(() => validateHello(unknownOptional))

  const badStatus = structuredClone(hello)
  badStatus.capabilities.agent.status = 'maybe'
  assert.throws(() => validateHello(badStatus), /status/)
})

test('hello ignores an unknown optional field but rejects a malformed one', () => {
  const extra = structuredClone(hello)
  extra.futureField = { anything: true }
  assert.doesNotThrow(() => validateHello(extra))
  const missingRoot = structuredClone(hello)
  delete missingRoot.rootSessionId
  assert.throws(() => validateHello(missingRoot), /rootSessionId/)
})

test('event and streamEnded enforce identity and generation', () => {
  const event = structuredClone(CONTRACT.fixtures.event.params)
  assert.doesNotThrow(() => validateEvent(event, { runtimeId: 'rt-fixture', generation: 1 }))
  assert.throws(
    () => validateEvent(event, { runtimeId: 'rt-fixture', generation: 2 }),
    (error) => error.code === 1005,
  )
  const unknownKind = structuredClone(event)
  unknownKind.kind = 'future.event'
  assert.throws(() => validateEvent(unknownKind), /unknown event kind/)

  const ended = structuredClone(CONTRACT.fixtures.streamEnded.params)
  assert.doesNotThrow(() => validateStreamEnded(ended, { runtimeId: 'rt-fixture', generation: 1 }))
  ended.outcome = 'maybe'
  assert.throws(() => validateStreamEnded(ended), /outcome/)
})

test('startupError rejects unknown codes', () => {
  const params = structuredClone(CONTRACT.fixtures.startupError.params)
  assert.doesNotThrow(() => validateStartupError(params))
  params.code = 'mystery'
  assert.throws(() => validateStartupError(params), /startup error code/)
})

test('encodeFrame adds a nonce only in fallback mode', () => {
  const frame = { jsonrpc: '2.0', method: 'nano.event', params: {} }
  const primary = encodeFrame(frame).toString('utf8')
  assert.equal(JSON.parse(primary).nonce, undefined)
  const fallback = JSON.parse(encodeFrame(frame, { requireNonce: true, nonce: 'n1' }).toString('utf8'))
  assert.equal(fallback.nonce, 'n1')
})

test('RpcError maps contract names to numeric codes', () => {
  const error = new RpcError('nano/not-ready', 'starting')
  assert.equal(error.code, 1001)
  assert.throws(() => new RpcError('missing/code', 'x'), /unknown protocol error name/)
})

test('invalid params keep the request id for the error response', () => {
  try {
    parseFrameLine(
      JSON.stringify({ jsonrpc: '2.0', id: 'req-9', method: 'nano.subscribe', params: [] }),
    )
    assert.fail('expected a ProtocolViolation')
  } catch (error) {
    assert.equal(error.code, -32602)
    assert.equal(error.requestId, 'req-9')
  }
})
