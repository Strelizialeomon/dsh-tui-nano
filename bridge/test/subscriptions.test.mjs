// S12 child-address subscriptions: official catalog authorization, child
// replay (including fork-inherited events), and per-address live filtering.
import test from 'node:test'
import assert from 'node:assert/strict'

import { SubscriptionHub } from '../runtime/events.mjs'

const ROOT = 'root'

function observation(events, cursor = events.length - 1) {
  const lease = {
    cursor,
    header: { id: 'child', createdAt: 1 },
    events,
    [Symbol.dispose]() {
      lease.disposed = true
    },
    disposed: false,
  }
  return lease
}

function harness({ query, authorizeChild } = {}) {
  const listeners = []
  const frames = []
  const ctx = {
    get(name) {
      return name === 'sessionQuery' ? query : undefined
    },
    on(event, handler) {
      listeners.push({ event, handler })
      return () => {}
    },
  }
  const writer = {
    enqueueEvent(id, buffer) {
      frames.push({ id, frame: JSON.parse(buffer.toString('utf8')) })
      return { ok: true }
    },
    dropSubscription() {},
    sendControl() {
      return Promise.resolve()
    },
  }
  const hub = new SubscriptionHub({
    ctx,
    writer,
    runtimeId: 'rt',
    generation: 1,
    rootSessionId: ROOT,
    session: { id: ROOT, seq: 10, header: { id: ROOT } },
    nonce: 'n',
    nonceRequired: false,
    authorizeChild,
  })
  const emit = (subject, event) => {
    for (const entry of listeners) {
      if (entry.event === 'session/event') entry.handler(subject, event)
    }
  }
  return { hub, frames, emit }
}

const childAddress = {
  rootSessionId: ROOT,
  parentSessionId: ROOT,
  childSessionId: 'child',
  mode: 'continuable',
}

function authorize(address) {
  return Promise.resolve(address)
}

test('a child subscription replays only that child and keeps the address', async () => {
  const query = {
    async observeSession(id) {
      assert.equal(id, 'child')
      return observation([
        { seq: 0, type: 'subagent/descriptor', data: { mode: 'continuable' } },
        { seq: 1, type: 'user/message', data: { text: 'inherited seed' } },
        { seq: 2, type: 'assistant/message', data: { text: 'child answer' } },
      ])
    },
  }
  const { hub, frames } = harness({ query, authorizeChild: authorize })
  const result = await hub.subscribe({ address: childAddress, kinds: ['session.event'], fromSeq: 0 })
  assert.equal(result.seq, 3)
  assert.deepEqual(result.address, childAddress)
  assert.equal(frames.length, 3)
  for (const entry of frames) {
    assert.equal(entry.frame.params.kind, 'session.event')
    assert.equal(entry.frame.params.payload.address.childSessionId, 'child')
  }
  // The fork seed is ordinary child history and is not suppressed.
  assert.ok(frames.some((entry) => JSON.stringify(entry.frame).includes('inherited seed')))
})

test('live child events reach only child subscriptions', async () => {
  const query = {
    async observeSession() {
      return observation([], 0)
    },
  }
  const { hub, frames, emit } = harness({ query, authorizeChild: authorize })
  await hub.subscribe({ address: childAddress, kinds: ['session.event'], fromSeq: 1 })
  await hub.subscribe({ address: { rootSessionId: ROOT }, kinds: ['session.event'], fromSeq: 11 })
  const before = frames.length
  emit({ id: 'child' }, { seq: 1, type: 'user/message', data: { text: 'child live' } })
  assert.equal(frames.length, before + 1)
  assert.equal(frames.at(-1).frame.params.payload.address.childSessionId, 'child')
  emit({ id: ROOT }, { seq: 11, type: 'user/message', data: { text: 'root live' } })
  assert.equal(frames.at(-1).frame.params.payload.address.childSessionId, undefined)
  // A sibling's event reaches neither subscription.
  const count = frames.length
  emit({ id: 'sibling' }, { seq: 1, type: 'user/message', data: { text: 'sibling' } })
  assert.equal(frames.length, count)
})

test('child subscriptions receive only their own live frames', async () => {
  const query = {
    async observeSession() {
      return observation([], 0)
    },
  }
  const { hub, frames } = harness({ query, authorizeChild: authorize })
  await hub.subscribe({
    address: { ...childAddress, childSessionId: 'child-a' },
    kinds: ['conversation.stream'],
    fromSeq: 1,
  })
  const before = frames.length
  hub.publish('conversation.stream', {
    address: { rootSessionId: ROOT },
    frame: { text: 'ROOT-SECRET' },
  })
  hub.publish('conversation.stream', {
    address: { rootSessionId: ROOT, parentSessionId: ROOT, childSessionId: 'child-b', mode: 'continuable' },
    frame: { text: 'SIBLING-SECRET' },
  })
  assert.equal(frames.length, before)
  hub.publish('conversation.stream', {
    address: { rootSessionId: ROOT, parentSessionId: ROOT, childSessionId: 'child-a', mode: 'continuable' },
    frame: { text: 'own' },
  })
  assert.equal(frames.length, before + 1)
  assert.ok(JSON.stringify(frames.at(-1).frame).includes('own'))
})

test('an unauthorized child address is rejected before replay', async () => {
  const query = {
    async observeSession() {
      throw new Error('must not be read')
    },
  }
  const { hub } = harness({
    query,
    authorizeChild: async () => {
      const error = new Error('not a direct child')
      error.rpcName = 'nano/subagents/unauthorized'
      throw error
    },
  })
  await assert.rejects(
    hub.subscribe({ address: childAddress, kinds: ['session.event'], fromSeq: 0 }),
    /not a direct child/,
  )
})
