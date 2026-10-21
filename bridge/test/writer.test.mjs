// Bounded writer tests: control priority, byte bounds, and flush timeout.
import test from 'node:test'
import assert from 'node:assert/strict'

import { BoundedWriter } from '../runtime/writer.mjs'

test('control frames jump ahead of queued events', async () => {
  const writes = []
  let releaseFirst
  const sink = (buffer) =>
    new Promise((resolve) => {
      writes.push(buffer.toString('utf8').trim())
      if (writes.length === 1) releaseFirst = resolve
      else resolve()
    })
  const writer = new BoundedWriter({
    write: sink,
    maxEventFrameBytes: 1024,
    maxSubscriptionQueueBytes: 4096,
    maxTotalQueueBytes: 8192,
  })
  assert.deepEqual(writer.enqueueEvent('sub-1', Buffer.from('event-1\n')), { ok: true })
  assert.deepEqual(writer.enqueueEvent('sub-1', Buffer.from('event-2\n')), { ok: true })
  const control = writer.sendControl(Buffer.from('control\n'))
  releaseFirst()
  await control
  await writer.flush()
  assert.deepEqual(writes, ['event-1', 'control', 'event-2'])
})

test('refuses an event frame above the per-event bound', async () => {
  const writer = new BoundedWriter({ write: async () => {}, maxEventFrameBytes: 4 })
  const admitted = writer.enqueueEvent('sub-1', Buffer.from('12345\n'))
  assert.deepEqual(admitted, { ok: false, reason: 'event-too-large' })
})

test('refuses events beyond the per-subscription byte bound', async () => {
  let release
  const gate = new Promise((resolve) => {
    release = resolve
  })
  const writer = new BoundedWriter({
    write: () => gate,
    maxEventFrameBytes: 1024,
    maxSubscriptionQueueBytes: 10,
    maxTotalQueueBytes: 100,
  })
  assert.deepEqual(writer.enqueueEvent('sub-1', Buffer.from('123456\n')), { ok: true })
  assert.deepEqual(writer.enqueueEvent('sub-1', Buffer.from('123456\n')), { ok: true })
  assert.deepEqual(writer.enqueueEvent('sub-1', Buffer.from('123456\n')), {
    ok: false,
    reason: 'slow-consumer',
  })
  release()
  await writer.flush()
})

test('flush rejects when the sink never drains', async () => {
  const writer = new BoundedWriter({ write: () => new Promise(() => {}), maxEventFrameBytes: 1024 })
  writer.enqueueEvent('sub-1', Buffer.from('event\n'))
  await assert.rejects(writer.flush({ timeoutMs: 20 }), /did not flush/)
})

test('a write failure settles the in-flight control promise', async () => {
  const writer = new BoundedWriter({
    write: async () => {
      throw new Error('EPIPE')
    },
  })
  const control = writer.sendControl(Buffer.from('hello\n'))
  await assert.rejects(control, /EPIPE/)
})

test('queued control frames reject when an earlier write fails', async () => {
  let calls = 0
  const writer = new BoundedWriter({
    write: async () => {
      calls += 1
      if (calls === 1) throw new Error('EPIPE')
      return undefined
    },
  })
  const first = writer.sendControl(Buffer.from('one\n'))
  const second = writer.sendControl(Buffer.from('two\n'))
  await assert.rejects(first, /EPIPE/)
  await assert.rejects(second, /EPIPE/)
})

test('control and event sends short-circuit after a transport failure', async () => {
  const writer = new BoundedWriter({
    write: async () => {
      throw new Error('EPIPE')
    },
  })
  await assert.rejects(writer.sendControl(Buffer.from('one\n')), /EPIPE/)
  await assert.rejects(writer.sendControl(Buffer.from('two\n')), /EPIPE/)
  assert.deepEqual(writer.enqueueEvent('sub-1', Buffer.from('event\n')), {
    ok: false,
    reason: 'transport-failed',
  })
})
