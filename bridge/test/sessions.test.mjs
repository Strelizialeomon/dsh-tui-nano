// S07 session host: official list facts, active-only rename, projection-only
// context values, and the official /compact command. The fakes expose only the
// official service surface the bridge is allowed to call.
import test from 'node:test'
import assert from 'node:assert/strict'

import { SessionsService } from '../sessions/service.mjs'

const ROOT = 'root-session'

function session(overrides = {}) {
  return {
    id: ROOT,
    seq: 10,
    header: { id: ROOT, createdAt: 1000, cwd: '/work', isSeeded: false },
    snapshotEvents: () => [],
    ...overrides,
  }
}

function fakeCtx(options = {}) {
  const listeners = []
  const ctx = {
    services: {
      sessionQuery: options.query,
      sessionProjections: options.projections,
      sessionProjectionCache: options.cache,
      sessionTitle: options.titles,
      commands: options.commands,
      ...options.services,
    },
    sessions: { get: options.getSession ?? (() => undefined) },
    agents: { get: options.getAgent ?? (() => undefined) },
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
  return ctx
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

function liveProjections(values, asOfSeq = 10) {
  return {
    snapshot(_session, keys) {
      const selected = {}
      for (const key of keys ?? Object.keys(values)) {
        if (values[key] !== undefined) selected[key] = values[key]
      }
      return { asOfSeq, values: selected }
    },
  }
}

test('list reports live facts and leaves cold activity unknown', async () => {
  const root = session()
  const query = {
    async listSessions() {
      return [
        { header: root.header, live: true, persisted: true, formatStatus: 'current' },
        {
          header: { id: 'cold', createdAt: 500, cwd: '/work', isSeeded: false },
          live: false,
          persisted: true,
        },
        {
          header: {
            id: 'child',
            createdAt: 400,
            parentSession: 'root-session',
            origin: 'subagent',
            isSeeded: false,
          },
          live: false,
          persisted: true,
        },
      ]
    },
  }
  const ctx = fakeCtx({
    query,
    getSession: (id) => (id === ROOT ? root : undefined),
    getAgent: (id) => (id === ROOT ? { session: root, status: 'running' } : undefined),
    projections: liveProjections({ title: '活动会话' }),
    cache: {
      cachedSnapshot(header) {
        if (header.id === 'cold') return { values: { title: '旧会话' } }
        return undefined
      },
      cachedPredecessorTitle() {
        return undefined
      },
    },
  })
  const service = new SessionsService({
    ctx,
    hub: fakeHub(),
    agent: { status: 'idle' },
    session: root,
    rootSessionId: ROOT,
  })
  const result = await service.list()
  const byId = Object.fromEntries(result.sessions.map((row) => [row.sessionId, row]))
  assert.equal(byId[ROOT].title, '活动会话')
  assert.equal(byId[ROOT].agentAvailable, true)
  assert.equal(byId[ROOT].running, true)
  assert.equal(byId[ROOT].current, true)
  assert.equal(byId[ROOT].blank, true)
  assert.equal(byId[ROOT].updatedAt, 1000)
  assert.equal(byId.cold.title, '旧会话')
  assert.equal(byId.cold.updatedAt, undefined)
  assert.equal(byId.cold.blank, undefined)
  assert.equal(byId.child.origin, 'subagent')
  assert.equal(byId.child.parentSessionId, ROOT)
  // The official query order is preserved (no synthetic re-sort).
  assert.deepEqual(
    result.sessions.map((row) => row.sessionId),
    [ROOT, 'cold', 'child'],
  )
  service.close()
})

test('live activity and blankness come from the official event fold', async () => {
  const events = [
    { type: 'turn/start', seq: 1, time: 1500, data: {} },
    {
      type: 'user/message',
      seq: 2,
      time: 1600,
      data: { id: 'm-1', source: { kind: 'user' }, content: [] },
    },
  ]
  const root = session({ snapshotEvents: () => events })
  const ctx = fakeCtx({
    query: { async listSessions() { return [{ header: root.header, live: true, persisted: true }] } },
    getSession: () => root,
    getAgent: () => ({ session: root, status: 'idle' }),
    projections: liveProjections({ title: null }),
  })
  const service = new SessionsService({
    ctx,
    hub: fakeHub(),
    agent: { status: 'idle' },
    session: root,
    rootSessionId: ROOT,
  })
  const [row] = (await service.list()).sessions
  assert.equal(row.blank, false)
  assert.equal(row.updatedAt, 1600)
  service.close()
})

test('rename is limited to the live root and passes official failures through', () => {
  const root = session()
  let renamed
  const ctx = fakeCtx({
    getSession: (id) => (id === ROOT ? root : undefined),
    titles: {
      rename(target, title) {
        renamed = { target, title }
        if (title.trim() === '') {
          const error = new Error('empty title')
          error.name = 'SessionTitleInvalidError'
          throw error
        }
        return { title: title.trim(), eventSeq: 12 }
      },
    },
  })
  const service = new SessionsService({
    ctx,
    hub: fakeHub(),
    agent: { status: 'idle' },
    session: root,
    rootSessionId: ROOT,
  })
  assert.deepEqual(service.rename({ sessionId: ROOT, title: ' 新名字 ' }), {
    title: '新名字',
    seq: 12,
  })
  assert.equal(renamed.target, root)
  assert.throws(
    () => service.rename({ sessionId: 'cold', title: 'x' }),
    (error) => error.rpcName === 'nano/sessions/not-active',
  )
  assert.throws(
    () => service.rename({ sessionId: ROOT, title: '   ' }),
    (error) => error.rpcName === 'nano/sessions/title-invalid',
  )
  service.close()
})

test('context keeps occupancy, billing usage, and provenance separate', () => {
  const root = session()
  const ctx = fakeCtx({
    getSession: () => root,
    projections: liveProjections({
      contextPressure: { projectedTokens: 12345, contextWindow: 128000, pressureTokens: 12000 },
      tokenUsage: {
        uncachedInputTokens: 100,
        outputTokens: 20,
        cacheReadTokens: 3,
        cacheWriteTokens: 4,
      },
      contextBreakdown: { systemTokens: 10, toolsTokens: 20, messageTokens: 30 },
    }),
  })
  const service = new SessionsService({
    ctx,
    hub: fakeHub(),
    agent: { status: 'idle' },
    session: root,
    rootSessionId: ROOT,
  })
  const meter = service.context()
  assert.equal(meter.projectedTokens, 12345)
  assert.equal(meter.contextWindow, 128000)
  assert.equal(meter.pressureTokens, 12000)
  assert.equal(meter.approximate, true)
  assert.equal(meter.tokenUsage.outputTokens, 20)
  assert.deepEqual(meter.contextBreakdown, { systemTokens: 10, toolsTokens: 20, messageTokens: 30 })
  assert.equal(meter.pressureAt, undefined)
  assert.equal(meter.contextWindowAt, undefined)

  // A missing window is omitted; the client reports unknown instead of zero.
  const missing = new SessionsService({
    ctx: fakeCtx({
      getSession: () => root,
      projections: liveProjections({
        contextPressure: { projectedTokens: 10 },
        tokenUsage: { uncachedInputTokens: 0, outputTokens: 0, cacheReadTokens: 0, cacheWriteTokens: 0 },
      }),
    }),
    hub: fakeHub(),
    agent: { status: 'idle' },
    session: root,
    rootSessionId: ROOT,
  })
  assert.equal(missing.context().contextWindow, undefined)
  missing.close()
  service.close()
})

test('context provenance times are taken from the official events', () => {
  const events = [
    { type: 'request/context', seq: 3, time: 2000, data: { contextWindow: 128000 } },
    {
      type: 'assistant/message',
      seq: 4,
      time: 2500,
      data: { usage: { inputTokens: 10, outputTokens: 5 }, message: { content: [] } },
    },
  ]
  const root = session({ snapshotEvents: () => events })
  const service = new SessionsService({
    ctx: fakeCtx({
      getSession: () => root,
      projections: liveProjections({
        contextPressure: { projectedTokens: 10, contextWindow: 128000 },
        tokenUsage: { uncachedInputTokens: 10, outputTokens: 5, cacheReadTokens: 0, cacheWriteTokens: 0 },
      }),
    }),
    hub: fakeHub(),
    agent: { status: 'idle' },
    session: root,
    rootSessionId: ROOT,
  })
  const meter = service.context()
  assert.equal(meter.contextWindowAt, 2000)
  assert.equal(meter.pressureAt, 2500)
  service.close()
})

test('context publications are coalesced and skip unchanged values', async () => {
  const root = session()
  const hub = fakeHub()
  const service = new SessionsService({
    ctx: fakeCtx({
      getSession: () => root,
      projections: liveProjections({
        contextPressure: { projectedTokens: 10, contextWindow: 100 },
        tokenUsage: { uncachedInputTokens: 0, outputTokens: 0, cacheReadTokens: 0, cacheWriteTokens: 0 },
      }),
    }),
    hub,
    agent: { status: 'idle' },
    session: root,
    rootSessionId: ROOT,
  })
  service.scheduleContextPublish()
  service.scheduleContextPublish()
  await new Promise((resolve) => setTimeout(resolve, 350))
  assert.equal(hub.published.length, 1)
  assert.equal(hub.published[0].kind, 'sessions.context')
  assert.equal(hub.published[0].payload.projectedTokens, 10)
  service.scheduleContextPublish()
  await new Promise((resolve) => setTimeout(resolve, 350))
  assert.equal(hub.published.length, 1, 'an unchanged projection is not republished')
  service.close()
})

test('compact uses the official command and passes its text through', async () => {
  const root = session()
  const calls = []
  const commands = {
    find(_agent, name) {
      return name === 'compact' ? { name } : undefined
    },
    async execute(_agent, line, attachments, signal) {
      calls.push({ line, attachments, aborted: signal.aborted })
      return {
        commandId: 'cmd-1',
        result: {
          kind: 'success',
          text: 'Compacted 2 history items (~40 tokens).',
          sourceEventSeq: 9,
        },
      }
    },
  }
  const service = new SessionsService({
    ctx: fakeCtx({ getSession: () => root, commands }),
    hub: fakeHub(),
    agent: { status: 'idle' },
    session: root,
    rootSessionId: ROOT,
  })
  const result = await service.compact({})
  assert.equal(calls[0].line, '/compact')
  assert.deepEqual(calls[0].attachments, [])
  assert.equal(result.kind, 'success')
  assert.equal(result.text, 'Compacted 2 history items (~40 tokens).')
  assert.equal(result.sourceEventSeq, 9)

  await service.compact({ input: 'now' })
  assert.equal(calls[1].line, '/compact now', 'arguments reach the official usage error path')
  service.close()
})

test('a missing command adapter keeps automatic compaction only', async () => {
  const root = session()
  const service = new SessionsService({
    ctx: fakeCtx({ getSession: () => root }),
    hub: fakeHub(),
    agent: { status: 'idle' },
    session: root,
    rootSessionId: ROOT,
  })
  await assert.rejects(
    () => service.compact({}),
    (error) => error.rpcName === 'nano/sessions/compact-unavailable',
  )
  service.close()
})

test('a command whose definition is absent is reported as unavailable', async () => {
  const root = session()
  const service = new SessionsService({
    ctx: fakeCtx({
      getSession: () => root,
      commands: {
        find: () => undefined,
        execute: async () => {
          throw new Error('must not run')
        },
      },
    }),
    hub: fakeHub(),
    agent: { status: 'idle' },
    session: root,
    rootSessionId: ROOT,
  })
  await assert.rejects(
    () => service.compact({}),
    (error) => error.rpcName === 'nano/sessions/compact-unavailable',
  )
  service.close()
})

test('a cancelled compact settles as the official cancellation text', async () => {
  const root = session()
  const service = new SessionsService({
    ctx: fakeCtx({
      getSession: () => root,
      commands: {
        find: () => ({ name: 'compact' }),
        async execute(_agent, _line, _attachments, signal) {
          // The official registry races the handler with the abort signal and
          // rejects before the handler's own `Compaction cancelled.` result.
          assert.equal(signal.aborted, true)
          const error = new Error('This operation was aborted')
          error.name = 'AbortError'
          throw error
        },
      },
    }),
    hub: fakeHub(),
    agent: { status: 'idle' },
    session: root,
    rootSessionId: ROOT,
  })
  const controller = new AbortController()
  controller.abort()
  const result = await service.compact({}, controller.signal)
  assert.equal(result.kind, 'error')
  assert.equal(result.text, 'Compaction cancelled.')
  assert.equal(result.commandId, undefined)
  service.close()
})

test('a missing token meter is unknown, not zero billing usage', () => {
  const root = session()
  const service = new SessionsService({
    ctx: fakeCtx({
      getSession: () => root,
      projections: liveProjections({
        contextPressure: { projectedTokens: 10, contextWindow: 100 },
      }),
    }),
    hub: fakeHub(),
    agent: { status: 'idle' },
    session: root,
    rootSessionId: ROOT,
  })
  const meter = service.context()
  assert.equal(meter.tokenUsage, undefined)
  assert.equal(meter.projectedTokens, 10)
  service.close()
})
