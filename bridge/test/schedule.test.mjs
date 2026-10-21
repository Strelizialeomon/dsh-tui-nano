// Schedule host tests: the per-session lease and the RPC surface over the
// official service. The fake service mirrors only the public methods S11 calls;
// no test reaches into the official scheduler.
import test from 'node:test'
import assert from 'node:assert/strict'
import fs from 'node:fs'
import os from 'node:os'
import path from 'node:path'
import { spawn } from 'node:child_process'
import { pathToFileURL } from 'node:url'

import {
  acquireScheduleLease,
  releaseScheduleLease,
  ScheduleLeaseError,
} from '../schedule/lease.mjs'
import { ScheduleBridge, scheduleRpcError } from '../schedule/service.mjs'

function tempRoot() {
  return fs.mkdtempSync(path.join(os.tmpdir(), 'nano-schedule-test-'))
}

function waitForExit(child) {
  return new Promise((resolve) => child.once('exit', resolve))
}

function sleepingChild() {
  return spawn(process.execPath, ['-e', 'setTimeout(() => {}, 30000)'], { stdio: 'ignore' })
}

test('lease creates a 0600 file inside the 0700 root run directory', (t) => {
  const dataRoot = tempRoot()
  t.after(() => fs.rmSync(dataRoot, { recursive: true, force: true }))
  const lease = acquireScheduleLease({ dataRoot, rootSessionId: 'session-root' })
  const directory = path.join(dataRoot, 'runtime-data', 'session-root')
  assert.equal(fs.statSync(directory).mode & 0o777, 0o700)
  assert.equal(fs.statSync(lease.file).mode & 0o777, 0o600)
  const content = JSON.parse(fs.readFileSync(lease.file, 'utf8'))
  assert.equal(content.sessionId, 'session-root')
  assert.equal(content.hostPid, process.pid)
  assert.equal(content.version, 1)
  assert.equal(releaseScheduleLease(lease), true)
  assert.equal(fs.existsSync(lease.file), false)
})

test('the same pid re-acquires idempotently', (t) => {
  const dataRoot = tempRoot()
  t.after(() => fs.rmSync(dataRoot, { recursive: true, force: true }))
  const first = acquireScheduleLease({ dataRoot, rootSessionId: 'session-root' })
  const second = acquireScheduleLease({ dataRoot, rootSessionId: 'session-root' })
  assert.equal(second.adopted, true)
  assert.equal(second.file, first.file)
  releaseScheduleLease(second)
})

test('a live owner blocks the second host and its lock is never deleted', async (t) => {
  const dataRoot = tempRoot()
  const child = sleepingChild()
  t.after(async () => {
    child.kill('SIGKILL')
    await waitForExit(child)
    fs.rmSync(dataRoot, { recursive: true, force: true })
  })
  const file = path.join(dataRoot, 'runtime-data', 'session-root', 'schedule.lock')
  fs.mkdirSync(path.dirname(file), { recursive: true, mode: 0o700 })
  fs.writeFileSync(
    file,
    JSON.stringify({ version: 1, sessionId: 'session-root', hostPid: child.pid, acquiredAt: new Date().toISOString() }),
    { mode: 0o600 },
  )
  assert.throws(
    () => acquireScheduleLease({ dataRoot, rootSessionId: 'session-root' }),
    (error) => error instanceof ScheduleLeaseError && error.code === 'held' && error.holderPid === child.pid,
  )
  assert.equal(fs.existsSync(file), true, 'a live lock must survive the refusal')
})

test('a dead owner is swept and the next host takes over', async (t) => {
  const dataRoot = tempRoot()
  t.after(() => fs.rmSync(dataRoot, { recursive: true, force: true }))
  const child = sleepingChild()
  await new Promise((resolve) => setTimeout(resolve, 100))
  const deadPid = child.pid
  child.kill('SIGKILL')
  await waitForExit(child)
  const file = path.join(dataRoot, 'runtime-data', 'session-root', 'schedule.lock')
  fs.mkdirSync(path.dirname(file), { recursive: true, mode: 0o700 })
  fs.writeFileSync(
    file,
    JSON.stringify({ version: 1, sessionId: 'session-root', hostPid: deadPid, acquiredAt: new Date().toISOString() }),
    { mode: 0o600 },
  )
  const lease = acquireScheduleLease({ dataRoot, rootSessionId: 'session-root' })
  const content = JSON.parse(fs.readFileSync(lease.file, 'utf8'))
  assert.equal(content.hostPid, process.pid)
  releaseScheduleLease(lease)
})

test('a corrupt lock is unknown while fresh and stale after 30 seconds', (t) => {
  const dataRoot = tempRoot()
  t.after(() => fs.rmSync(dataRoot, { recursive: true, force: true }))
  const file = path.join(dataRoot, 'runtime-data', 'session-root', 'schedule.lock')
  fs.mkdirSync(path.dirname(file), { recursive: true, mode: 0o700 })
  fs.writeFileSync(file, 'not json', { mode: 0o600 })
  assert.throws(
    () => acquireScheduleLease({ dataRoot, rootSessionId: 'session-root' }),
    (error) => error instanceof ScheduleLeaseError && error.code === 'unknown',
  )
  const stale = new Date(Date.now() - 31_000)
  fs.utimesSync(file, stale, stale)
  const lease = acquireScheduleLease({ dataRoot, rootSessionId: 'session-root' })
  assert.equal(lease.adopted, false)
  releaseScheduleLease(lease)
})

test('release never removes a lock owned by another pid', (t) => {
  const dataRoot = tempRoot()
  t.after(() => fs.rmSync(dataRoot, { recursive: true, force: true }))
  const lease = acquireScheduleLease({ dataRoot, rootSessionId: 'session-root' })
  fs.writeFileSync(
    lease.file,
    JSON.stringify({ version: 1, sessionId: 'session-root', hostPid: process.pid + 1, acquiredAt: new Date().toISOString() }),
    { mode: 0o600 },
  )
  assert.equal(releaseScheduleLease(lease), false)
  assert.equal(fs.existsSync(lease.file), true)
})

test('atomic publication leaves no temp files behind', (t) => {
  const dataRoot = tempRoot()
  t.after(() => fs.rmSync(dataRoot, { recursive: true, force: true }))
  const lease = acquireScheduleLease({ dataRoot, rootSessionId: 'session-root' })
  const files = fs.readdirSync(path.dirname(lease.file))
  assert.deepEqual(files, ['schedule.lock'])
  releaseScheduleLease(lease)
})

test('two hosts reclaiming one dead lock produce at most one owner', async (t) => {
  const dataRoot = tempRoot()
  t.after(() => fs.rmSync(dataRoot, { recursive: true, force: true }))
  const child = sleepingChild()
  await new Promise((resolve) => setTimeout(resolve, 100))
  const deadPid = child.pid
  child.kill('SIGKILL')
  await waitForExit(child)
  const file = path.join(dataRoot, 'runtime-data', 'session-root', 'schedule.lock')
  fs.mkdirSync(path.dirname(file), { recursive: true, mode: 0o700 })
  fs.writeFileSync(
    file,
    JSON.stringify({ version: 1, sessionId: 'session-root', hostPid: deadPid, acquiredAt: new Date().toISOString() }),
    { mode: 0o600 },
  )
  const leaseModule = pathToFileURL(path.join(import.meta.dirname, '..', 'schedule', 'lease.mjs')).href
  const script = `
    import { acquireScheduleLease } from ${JSON.stringify(leaseModule)}
    try {
      const lease = acquireScheduleLease({ dataRoot: process.argv[1], rootSessionId: 'session-root' })
      console.log('OK ' + lease.hostPid)
      await new Promise((resolve) => setTimeout(resolve, 400))
    } catch (error) {
      console.log('ERR ' + (error.code ?? '') + ' ' + (error.message ?? ''))
    }
  `
  const children = [0, 1].map(() =>
    spawn(process.execPath, ['--input-type=module', '-e', script, dataRoot], {
      stdio: ['ignore', 'pipe', 'pipe'],
    }),
  )
  const outputs = await Promise.all(
    children.map(
      (runner) =>
        new Promise((resolve) => {
          let text = ''
          runner.stdout.on('data', (chunk) => {
            text += chunk.toString()
          })
          runner.once('exit', () => resolve(text.trim()))
        }),
    ),
  )
  const owners = outputs.filter((line) => line.startsWith('OK '))
  assert.equal(owners.length, 1, `exactly one host may own the table; got ${JSON.stringify(outputs)}`)
  const losers = outputs.filter((line) => !line.startsWith('OK '))
  assert.equal(losers.length, 1)
  assert.match(losers[0], /^ERR (held|unknown)\b/)
})

function fakeContext() {
  const handlers = new Map()
  return {
    on(event, handler) {
      const list = handlers.get(event) ?? []
      list.push(handler)
      handlers.set(event, list)
      return () => {
        const current = handlers.get(event) ?? []
        handlers.set(event, current.filter((item) => item !== handler))
      }
    },
    emit(event, payload) {
      for (const handler of [...(handlers.get(event) ?? [])]) handler(payload)
    },
  }
}

function fakeHub() {
  return { published: [], publish(kind, payload) { this.published.push({ kind, payload }) } }
}

function fakeSchedule() {
  const calls = {}
  return {
    calls,
    async catalog() {
      return [{ id: 's-1', kind: 'every', title: 'T', prompt: 'P', scheduledAt: '2099-01-01T00:00:00.000Z', sessionId: 'root', status: 'active' }]
    },
    async list(request) {
      calls.list = request
      return [{ id: 's-1', kind: 'every', title: 'T', prompt: 'P', scheduledAt: '2099-01-01T00:00:00.000Z' }]
    },
    async create(sessionId, request, signal) {
      calls.create = { sessionId, request, signal }
      return { id: 's-1', ...request }
    },
    async update(request, signal) {
      calls.update = { request, signal }
      return { id: request.id, updated: true, record: request.expected }
    },
    async delete(request, signal) {
      calls.delete = { request, signal }
      return { id: request.id, deleted: true }
    },
    async history(request) {
      calls.history = request
      return {
        id: request.id,
        records: [],
        earlierRecordsUnavailable: false,
        earlierRecordsPruned: false,
        retention: { days: 30, records: 200 },
      }
    },
  }
}

function bridgeFor(schedule) {
  const ctx = fakeContext()
  const hub = fakeHub()
  const bridge = new ScheduleBridge({ ctx, hub, schedule, rootSessionId: 'root' })
  return { ctx, hub, bridge }
}

test('the RPC surface delegates with the root session and forwards changes', async () => {
  const schedule = fakeSchedule()
  const { ctx, hub, bridge } = bridgeFor(schedule)
  const address = { rootSessionId: 'root' }

  const catalog = await bridge.catalog({ address })
  assert.equal(catalog.address.rootSessionId, 'root')
  assert.equal(catalog.entries.length, 1)

  const list = await bridge.list({ address })
  assert.equal(list.records.length, 1)
  assert.equal(schedule.calls.list.sessionId, 'root')

  const created = await bridge.create({ address, request: { title: 'T', prompt: 'P', after_seconds: 60 } })
  assert.equal(created.record.id, 's-1')
  assert.equal(schedule.calls.create.sessionId, 'root')

  const expected = { id: 's-1', kind: 'every', title: 'T', prompt: 'P', scheduledAt: '2099-01-01T00:00:00.000Z', everySeconds: 60 }
  const updated = await bridge.update({
    address,
    id: 's-1',
    expected,
    title: 'T2',
    change: { kind: 'every', every_seconds: 90 },
  })
  assert.equal(updated.result.updated, true)
  assert.deepEqual(schedule.calls.update.request.expected, expected)
  assert.equal(schedule.calls.update.request.sessionId, 'root')
  assert.equal(schedule.calls.update.request.title, 'T2')

  const deleted = await bridge.delete({ address, id: 's-1' })
  assert.equal(deleted.result.deleted, true)

  const history = await bridge.history({ address, id: 's-1', limit: 50 })
  assert.equal(history.result.retention.days, 30)
  assert.equal(history.result.records.length, 0)

  ctx.emit('schedule/changed')
  assert.deepEqual(hub.published, [{ kind: 'schedule.changed', payload: { address: { rootSessionId: 'root' } } }])
  bridge.close()
  ctx.emit('schedule/changed')
  assert.equal(hub.published.length, 1)
})

test('a foreign root is refused before the official service is touched', async () => {
  const schedule = fakeSchedule()
  const { bridge } = bridgeFor(schedule)
  await assert.rejects(() => bridge.catalog({ address: { rootSessionId: 'other' } }), (error) => error.code === 1008)
  await assert.rejects(
    () => bridge.list({ address: { rootSessionId: 'root', childSessionId: 'child-1' } }),
    (error) => error.code === 1008,
  )
})

test('official validation errors keep the stable code in the RPC error', () => {
  const mapped = scheduleRpcError(Object.assign(new Error('bad cron'), { code: 'invalid_rule' }))
  assert.equal(mapped.rpcName, 'nano/schedule/error')
  assert.equal(mapped.code, 1030)
  assert.equal(mapped.data.code, 'invalid_rule')
  assert.match(mapped.message, /invalid_rule/)
})

test('history forwards the explicit limit and cursor', async () => {
  const schedule = fakeSchedule()
  const { bridge } = bridgeFor(schedule)
  await bridge.history({ address: { rootSessionId: 'root' }, id: 's-1', limit: 25, before: 'm-9' })
  assert.deepEqual(schedule.calls.history, {
    sessionId: 'root',
    id: 's-1',
    limit: 25,
    before: 'm-9',
  })
})

test('the post-flush write-failure fixture keeps the task active with no receipt', () => {
  const fixture = JSON.parse(
    fs.readFileSync(new URL('./fixtures/schedule-post-flush-write-failure.json', import.meta.url), 'utf8'),
  )
  assert.equal(fixture.catalog.entries[0].status, 'active')
  assert.equal(fixture.catalog.entries[0].lastDelivery, undefined)
  assert.deepEqual(fixture.history.records, [])
  assert.equal(fixture.history.earlierRecordsUnavailable, false)
  assert.equal(fixture.history.earlierRecordsPruned, false)
})
