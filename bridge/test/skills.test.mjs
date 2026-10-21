// S05 unit tests. The fake registry, command runtime, and Session mirror only
// the official surfaces this service is allowed to use, so an accidental
// dependency on private discovery internals fails here.
import test from 'node:test'
import assert from 'node:assert/strict'
import fs from 'node:fs/promises'
import os from 'node:os'
import path from 'node:path'

import { SkillsService, utf8Boundary } from '../skills/service.mjs'

const ROOT = 'session-root'

function fakeContext({ skills, commands, get } = {}) {
  const handlers = new Map()
  const services = { skills, commands, ...get }
  return {
    handlers,
    get(name) {
      return services[name]
    },
    on(event, handler) {
      const list = handlers.get(event) ?? []
      list.push(handler)
      handlers.set(event, list)
      return () => {
        const current = handlers.get(event) ?? []
        handlers.set(event, current.filter((item) => item !== handler))
      }
    },
    emit(event, first, second) {
      for (const handler of [...(handlers.get(event) ?? [])]) handler(first, second)
    },
  }
}

function fakeSession(events = []) {
  return {
    seq: 3,
    snapshotEvents() {
      return events
    },
  }
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

function summary(overrides = {}) {
  return {
    name: 'hello',
    description: 'says hello',
    invocation: { modelInvocable: true, userInvocable: true },
    source: 'project-agents',
    provider: 'filesystem',
    path: '/tmp/hello/SKILL.md',
    ...overrides,
  }
}

function serviceFor(options = {}) {
  const { snapshot, commands = [], events = [], cwd = '/work' } = options
  const registry = 'skillsGet' in options
    ? options.skillsGet
    : {
        async snapshot() {
          return snapshot ?? { skills: [], complete: true }
        },
      }
  const ctx = fakeContext({ skills: registry, commands: commands.length === 0 ? undefined : { list: () => commands } })
  const hub = fakeHub()
  const session = fakeSession(events)
  const service = new SkillsService({ ctx, hub, agent: { id: ROOT }, session, rootSessionId: ROOT, cwd })
  return { ctx, hub, session, service }
}

const address = { rootSessionId: ROOT }

test('list keeps only user-invocable skills and records model-only restrictions', async () => {
  const { service } = serviceFor({
    snapshot: {
      complete: true,
      skills: [
        summary(),
        summary({
          name: 'user-only',
          invocation: { modelInvocable: false, userInvocable: true },
        }),
        summary({ name: 'model-only', invocation: { modelInvocable: true, userInvocable: false } }),
        summary({ name: 'no-path-skill', path: undefined, whenToUse: 'when testing' }),
      ],
    },
    commands: [{ name: 'compact' }],
  })
  const result = await service.list({ address })
  assert.equal(result.complete, true)
  assert.deepEqual(result.commands, ['compact'])
  assert.deepEqual(
    result.skills.map((skill) => skill.name),
    ['hello', 'user-only', 'no-path-skill'],
  )
  assert.equal(result.skills[1].modelInvocable, false)
  assert.equal(result.skills[0].modelInvocable, true)
  assert.equal(result.skills[2].whenToUse, 'when testing')
  assert.equal('path' in result.skills[2], false)
})

test('a command with the same name is reported for command-first adjudication', async () => {
  const { service } = serviceFor({
    snapshot: { complete: true, skills: [summary({ name: 'compact' })] },
    commands: [{ name: 'compact' }],
  })
  const result = await service.list({ address })
  assert.equal(result.skills[0].shadowedByCommand, true)
})

test('list reports an incomplete official observation instead of inventing a catalog', async () => {
  const { service } = serviceFor({
    snapshot: { complete: false, skills: [summary()] },
  })
  const result = await service.list({ address })
  assert.equal(result.complete, false)
  assert.equal(result.skills.length, 1)
})

test('list fails with nano/unsupported when the registry is absent', async () => {
  const { service } = serviceFor({ skillsGet: undefined })
  await assert.rejects(service.list({ address }), (error) => error.code === 1008)
})

test('preview reads the official path without touching the session', async () => {
  const directory = await fs.mkdtemp(path.join(os.tmpdir(), 'nano-s05-'))
  const file = path.join(directory, 'SKILL.md')
  await fs.writeFile(file, '# hello\n')
  const { service, hub } = serviceFor({
    snapshot: { complete: true, skills: [summary({ path: file })] },
  })
  const result = await service.preview({ address, name: 'hello' })
  assert.equal(result.path, file)
  assert.equal(result.text, '# hello\n')
  assert.equal(result.truncated, false)
  assert.equal(result.bytes, 8)
  assert.equal(result.limit, 65536)
  assert.deepEqual(hub.published, [])
  await fs.rm(directory, { recursive: true, force: true })
})

test('preview truncates at a UTF-8 boundary and says so', async () => {
  const directory = await fs.mkdtemp(path.join(os.tmpdir(), 'nano-s05-'))
  const file = path.join(directory, 'SKILL.md')
  const prefix = 'x'.repeat(65535)
  await fs.writeFile(file, `${prefix}汉`)
  const { service } = serviceFor({
    snapshot: { complete: true, skills: [summary({ path: file })] },
  })
  const result = await service.preview({ address, name: 'hello' })
  assert.equal(result.truncated, true)
  assert.equal(result.bytes, 65535)
  assert.equal(result.text, prefix)
  await fs.rm(directory, { recursive: true, force: true })
})

test('preview distinguishes unknown, pathless, and unreadable skills', async () => {
  const { service } = serviceFor({
    snapshot: {
      complete: true,
      skills: [summary({ name: 'virtual', path: undefined })],
    },
  })
  await assert.rejects(service.preview({ address, name: 'missing' }), (error) => error.code === 1019)
  await assert.rejects(service.preview({ address, name: 'virtual' }), (error) => error.code === 1020)
  const directory = await fs.mkdtemp(path.join(os.tmpdir(), 'nano-s05-'))
  const missing = path.join(directory, 'gone.md')
  const { service: unreadable } = serviceFor({
    snapshot: { complete: true, skills: [summary({ path: missing })] },
  })
  await assert.rejects(unreadable.preview({ address, name: 'hello' }), (error) => error.code === 1021)
  await fs.rm(directory, { recursive: true, force: true })
})

test('instructions are folded from durable agent-instructions messages in load order', () => {
  const events = [
    { type: 'user/message', time: 10, data: { source: { kind: 'user', rpcId: 'r' }, content: [] } },
    {
      type: 'user/message',
      time: 20,
      data: {
        source: {
          kind: 'agent-instructions',
          baseline: true,
          changes: [
            { action: 'set', path: '~/.agents/AGENTS.md' },
            { action: 'set', path: 'AGENTS.md' },
          ],
        },
        content: [{ type: 'text', text: '<system-reminder>\nbody\n</system-reminder>' }],
      },
    },
  ]
  const { service } = serviceFor({ events })
  const snapshot = service.snapshot({ address })
  assert.equal(snapshot.available, true)
  assert.equal(snapshot.baseline, true)
  assert.equal(snapshot.loadedAt, 20)
  assert.deepEqual(snapshot.sources, [
    { path: '~/.agents/AGENTS.md', action: 'set' },
    { path: 'AGENTS.md', action: 'set' },
  ])
  assert.equal('budgetNotice' in snapshot, false)
})

test('a later baseline replaces the source set instead of merging it', () => {
  const { service } = serviceFor({
    events: [
      instructionEvent(10, [{ action: 'set', path: 'old/AGENTS.md' }], true),
      instructionEvent(20, [{ action: 'set', path: 'AGENTS.md' }], true),
    ],
  })
  const snapshot = service.snapshot({ address })
  assert.deepEqual(snapshot.sources, [{ path: 'AGENTS.md', action: 'set' }])
  assert.equal(snapshot.loadedAt, 20)
})

test('delta changes add, replace, and remove sources; budget notices are visible', () => {
  const { service } = serviceFor({
    events: [
      instructionEvent(10, [
        { action: 'set', path: 'AGENTS.md' },
        { action: 'set', path: 'sub/AGENTS.md' },
      ], true),
      instructionEvent(
        20,
        [
          { action: 'replace', path: 'sub/AGENTS.md' },
          { action: 'remove', path: 'AGENTS.md' },
          { action: 'set', path: 'sub/CLAUDE.md' },
        ],
        false,
        'Workspace instruction budget 65536 bytes: omitted ~/.agents/AGENTS.md',
      ),
    ],
  })
  const snapshot = service.snapshot({ address })
  assert.deepEqual(snapshot.sources, [
    { path: 'sub/AGENTS.md', action: 'replace' },
    { path: 'sub/CLAUDE.md', action: 'set' },
  ])
  assert.equal('baseline' in snapshot, false)
  assert.equal(snapshot.loadedAt, 20)
  assert.equal(snapshot.budgetNotice, 'Workspace instruction budget 65536 bytes: omitted ~/.agents/AGENTS.md')
})

test('live instruction messages publish the full projection and ignore other sessions', () => {
  const { ctx, hub, service } = serviceFor()
  ctx.emit('session/event', { id: 'other' }, { type: 'user/message', time: 1, data: instructionData() })
  assert.deepEqual(hub.published, [])
  ctx.emit('session/event', { id: ROOT }, { type: 'user/message', time: 5, data: instructionData() })
  assert.equal(hub.published.length, 1)
  assert.equal(hub.published[0].kind, 'instructions.changed')
  assert.deepEqual(hub.published[0].payload.sources, [{ path: 'AGENTS.md', action: 'set' }])
  ctx.emit('session/event', { id: ROOT }, { type: 'assistant/message', time: 6, data: {} })
  assert.equal(hub.published.length, 1)
})

test('provider invalidation and command changes publish only a refresh signal', () => {
  const { ctx, hub, service } = serviceFor()
  ctx.emit('skills/change')
  ctx.emit('commands/change')
  assert.deepEqual(
    hub.published.map((item) => [item.kind, item.payload.reason]),
    [
      ['skills.changed', 'skills'],
      ['skills.changed', 'commands'],
    ],
  )
  assert.equal(service.instructions.available, false)
})

test('close releases listeners and stops publishing', () => {
  const { ctx, hub, service } = serviceFor()
  service.close()
  ctx.emit('skills/change')
  ctx.emit('session/event', { id: ROOT }, { type: 'user/message', time: 5, data: instructionData() })
  assert.deepEqual(hub.published, [])
})

test('foreign and child addresses are rejected', async () => {
  const { service } = serviceFor()
  await assert.rejects(service.list({ address: { rootSessionId: 'other' } }), (error) => error.code === 1011)
  await assert.rejects(
    service.list({ address: { rootSessionId: ROOT, childSessionId: 'child' } }),
    (error) => error.code === 1008,
  )
})

test('utf8Boundary never cuts a multi-byte character', () => {
  const bytes = Buffer.from('a汉')
  assert.equal(utf8Boundary(bytes, 1), 1)
  assert.equal(utf8Boundary(bytes, 2), 1)
  assert.equal(utf8Boundary(bytes, 3), 1)
  assert.equal(utf8Boundary(bytes, 4), 4)
})

function instructionEvent(time, changes, baseline, notice) {
  const text = notice === undefined ? '<system-reminder>\nbody\n</system-reminder>' : `<system-reminder>\n${notice}\nbody\n</system-reminder>`
  return {
    type: 'user/message',
    time,
    data: {
      source: { kind: 'agent-instructions', ...(baseline ? { baseline: true } : {}), changes },
      content: [{ type: 'text', text }],
    },
  }
}

function instructionData() {
  return {
    source: { kind: 'agent-instructions', baseline: true, changes: [{ action: 'set', path: 'AGENTS.md' }] },
    content: [{ type: 'text', text: '<system-reminder>\nbody\n</system-reminder>' }],
  }
}
