// Real-host schedule e2e: official CRUD, validation codes, compare-and-set
// conflicts, delivery receipts, cold catch-up, the schedule lease, and the
// storage-route refusal. Requires NANO_E2E=1 because it installs a profile and
// starts dsh. The recurring catch-up case waits one real minute.
import test from 'node:test'
import assert from 'node:assert/strict'
import fs from 'node:fs'
import os from 'node:os'
import path from 'node:path'
import { spawn, execFileSync } from 'node:child_process'
import { randomUUID } from 'node:crypto'
import { fileURLToPath } from 'node:url'

const bridgeRoot = fileURLToPath(new URL('../..', import.meta.url))
const installScript = path.join(bridgeRoot, 'profile', 'install.mjs')
const DSH = execFileSync('which', ['dsh'], { encoding: 'utf8' }).trim()

function tempRoot() {
  return fs.mkdtempSync(path.join(os.tmpdir(), 'nano-schedule-e2e-'))
}

function installProfile(dshHome, dataRoot) {
  const output = execFileSync(
    process.execPath,
    [
      installScript,
      '--dsh-home',
      dshHome,
      '--data-root',
      dataRoot,
      '--bridge-package',
      bridgeRoot,
      '--dsh-bin',
      DSH,
      '--json',
    ],
    { encoding: 'utf8' },
  )
  assert.equal(JSON.parse(output.trim()).installed, true)
}

function writePatch({ root, dataRoot, sessionId, runtimeId, work, mode = 'create', route = 'nano-schedule-json' }) {
  const patchPath = path.join(root, 'patch.json')
  fs.mkdirSync(path.dirname(patchPath), { recursive: true, mode: 0o700 })
  fs.writeFileSync(
    patchPath,
    JSON.stringify([
      { id: 'session-persistence-jsonl', config: { root: path.join(dataRoot, 'sessions') } },
      { id: 'storage-json', config: { root: path.join(dataRoot, 'runtime-data', sessionId, 'storage') } },
      {
        id: 'storage-domain',
        config: {
          backend: 'json',
          routes: { schedule: route, nano_probe: route },
        },
      },
      {
        id: 'nano-bridge',
        config: {
          runtimeId,
          generation: 1,
          rootSessionId: sessionId,
          cwd: work,
          dataRoot,
          sessionMode: mode,
          officialVersion: '0.2.1-rc.2',
          nonce: `nonce-${randomUUID()}`,
          askUserMode: 'blocking',
        },
      },
    ]),
    { mode: 0o600 },
  )
  return patchPath
}

function startHost({ dshHome, patchPath, work }) {
  const child = spawn(process.execPath, [DSH, '--profile', 'dsh-tui-nano', '--patch', patchPath], {
    cwd: work,
    env: { ...process.env, DSH_HOME: dshHome },
    stdio: ['ignore', 'pipe', 'pipe', 'pipe', 'pipe'],
  })
  const state = {
    child,
    frames: [],
    waiters: [],
    stderr: '',
    counter: 0,
    buffer: Buffer.alloc(0),
  }
  child.stderr.on('data', (chunk) => {
    state.stderr += chunk.toString()
  })
  child.stdio[4].on('data', (chunk) => {
    state.buffer = Buffer.concat([state.buffer, chunk])
    while (true) {
      const index = state.buffer.indexOf(0x0a)
      if (index < 0) break
      const line = state.buffer.subarray(0, index).toString('utf8')
      state.buffer = state.buffer.subarray(index + 1)
      if (line.trim() === '') continue
      let frame
      try {
        frame = JSON.parse(line)
      } catch {
        continue
      }
      state.frames.push(frame)
      for (const waiter of [...state.waiters]) {
        if (waiter.match(frame)) {
          state.waiters.splice(state.waiters.indexOf(waiter), 1)
          clearTimeout(waiter.timer)
          waiter.resolve(frame)
        }
      }
    }
  })
  state.send = (method, params) => {
    const id = `req-${++state.counter}`
    child.stdio[3].write(`${JSON.stringify({ jsonrpc: '2.0', id, method, params })}\n`)
    return id
  }
  state.call = async (method, params, timeout = 60_000) => {
    const id = state.send(method, params)
    return state.waitFor((frame) => frame.id === id, timeout)
  }
  state.waitFor = (match, timeout = 60_000) =>
    new Promise((resolve, reject) => {
      const existing = state.frames.find(match)
      if (existing !== undefined) return resolve(existing)
      const waiter = { match, resolve }
      waiter.timer = setTimeout(() => {
        state.waiters.splice(state.waiters.indexOf(waiter), 1)
        reject(
          new Error(
            `timed out waiting for a frame; recent=${JSON.stringify(state.frames.slice(-6))}; stderr=${state.stderr.slice(-2000)}`,
          ),
        )
      }, timeout)
      state.waiters.push(waiter)
    })
  state.stop = async () => {
    if (child.exitCode !== null) return child.exitCode
    const id = state.send('nano.shutdown', {})
    await state.waitFor((frame) => frame.id === id, 20_000).catch(() => undefined)
    return new Promise((resolve) => {
      if (child.exitCode !== null) return resolve(child.exitCode)
      const timer = setTimeout(() => {
        child.kill('SIGKILL')
        resolve('timeout')
      }, 15_000)
      child.once('exit', (code) => {
        clearTimeout(timer)
        resolve(code)
      })
    })
  }
  return state
}

async function boot(options) {
  const host = startHost(options)
  const hello = await host.waitFor(
    (frame) => frame.method === 'nano.hello' || frame.method === 'nano.startupError',
    90_000,
  )
  return { host, hello }
}

function address(sessionId) {
  return { rootSessionId: sessionId }
}

test(
  'official CRUD, delivery, cold catch-up, lease, and route refusal',
  { skip: process.env.NANO_E2E !== '1' ? 'set NANO_E2E=1' : false, timeout: 300_000 },
  async (t) => {
    const root = tempRoot()
    const dshHome = path.join(root, 'dsh-home')
    const dataRoot = path.join(root, 'data')
    const work = path.join(root, 'work')
    fs.mkdirSync(work, { recursive: true })
    installProfile(dshHome, dataRoot)
    const sessionId = `session-${randomUUID()}`
    const patch = writePatch({ root, dataRoot, sessionId, runtimeId: `rt-${randomUUID()}`, work })
    t.after(() => fs.rmSync(root, { recursive: true, force: true }))
    let host = startHost({ dshHome, patchPath: patch, work })
    t.after(() => {
      if (host.child.exitCode === null) host.child.kill('SIGKILL')
    })

    const hello = await host.waitFor(
      (frame) => frame.method === 'nano.hello' || frame.method === 'nano.startupError',
      90_000,
    )
    assert.equal(hello.method, 'nano.hello', `startup error: ${JSON.stringify(hello)}`)
    assert.equal(hello.params.capabilities.schedule.status, 'supported')

    const addr = address(sessionId)
    const created = []
    const selectors = [
      { after_seconds: 3600 },
      { at: new Date(Date.now() + 3_600_000).toISOString() },
      { every_seconds: 3600 },
      { daily: { time: '09:00:00', time_zone: 'Asia/Shanghai' } },
      { weekly: { time: '09:00:00', time_zone: 'Asia/Shanghai', weekdays: [1, 3] } },
      { cron: { expression: '*/15 9-17 * * 1-5', time_zone: 'Asia/Shanghai' } },
    ]
    for (const [index, selector] of selectors.entries()) {
      const reply = await host.call('nano.schedule.create', {
        address: addr,
        request: { title: `任务 ${index}`, prompt: `提醒 ${index}`, ...selector },
      })
      assert.ok(reply.result.record.id, `create ${index} failed: ${JSON.stringify(reply)}`)
      assert.equal(reply.result.record.title, `任务 ${index}`)
      created.push(reply.result.record)
    }

    const invalidTitle = await host.call('nano.schedule.create', {
      address: addr,
      request: { title: '   ', prompt: 'x', after_seconds: 60 },
    })
    assert.equal(invalidTitle.error.code, 1030)
    assert.match(invalidTitle.error.message, /invalid_prompt/)
    assert.equal(invalidTitle.error.data.code, 'invalid_prompt')

    const invalidPrompt = await host.call('nano.schedule.create', {
      address: addr,
      request: { title: '空白指令', prompt: '   ', after_seconds: 60 },
    })
    assert.equal(invalidPrompt.error.code, 1030)
    assert.equal(invalidPrompt.error.data.code, 'invalid_prompt')

    const invalidZone = await host.call('nano.schedule.create', {
      address: addr,
      request: {
        title: '坏时区',
        prompt: 'x',
        daily: { time: '09:00:00', time_zone: 'Mars/Olympus' },
      },
    })
    assert.equal(invalidZone.error.code, 1030)
    assert.equal(invalidZone.error.data.code, 'invalid_time_zone')

    const invalidCron = await host.call('nano.schedule.create', {
      address: addr,
      request: { title: 'bad cron', prompt: 'x', cron: { expression: '61 * * * *', time_zone: 'Asia/Shanghai' } },
    })
    assert.equal(invalidCron.error.code, 1030)
    assert.equal(invalidCron.error.data.code, 'invalid_rule')
    assert.match(invalidCron.error.message, /minute/)

    const catalog = await host.call('nano.schedule.catalog', { address: addr })
    assert.equal(catalog.result.entries.length, 6)
    assert.ok(catalog.result.entries.every((entry) => entry.status === 'active'))

    const activeOnly = await host.call('nano.schedule.list', { address: addr })
    assert.equal(activeOnly.result.records.length, 6)

    const history = await host.call('nano.schedule.history', {
      address: addr,
      id: created[0].id,
      limit: 50,
    })
    assert.deepEqual(history.result.result.records, [])
    assert.equal(history.result.result.earlierRecordsUnavailable, false)
    assert.equal(history.result.result.earlierRecordsPruned, false)

    const badLimit = await host.call('nano.schedule.history', {
      address: addr,
      id: created[0].id,
      limit: 0,
    })
    assert.equal(badLimit.error.data.code, 'invalid_rule')

    const expected = created[0]
    const updated = await host.call('nano.schedule.update', {
      address: addr,
      id: expected.id,
      expected,
      title: '改过的标题',
    })
    assert.equal(updated.result.result.updated, true)
    assert.equal(updated.result.result.record.title, '改过的标题')

    const conflicted = await host.call('nano.schedule.update', {
      address: addr,
      id: expected.id,
      expected,
      title: '并发覆盖',
    })
    assert.equal(conflicted.result.result.updated, false)
    assert.equal(conflicted.result.result.code, 'schedule_conflict')
    const afterConflict = await host.call('nano.schedule.catalog', { address: addr })
    const kept = afterConflict.result.entries.find((entry) => entry.id === expected.id)
    assert.equal(kept.title, '改过的标题', 'a conflict must not overwrite the committed edit')

    const removed = await host.call('nano.schedule.delete', { address: addr, id: created[2].id })
    assert.equal(removed.result.result.deleted, true)
    const historyAfterDelete = await host.call('nano.schedule.history', {
      address: addr,
      id: created[2].id,
      limit: 10,
    })
    assert.equal(historyAfterDelete.result.result.code, 'schedule_not_found')
    const removedAgain = await host.call('nano.schedule.delete', { address: addr, id: created[2].id })
    assert.equal(removedAgain.result.result.deleted, false)

    // One-shot delivery: the reminder becomes a user message in the original
    // session and a receipt is saved. The receipt means inbox entry.
    const due = await host.call('nano.schedule.create', {
      address: addr,
      request: {
        title: '两秒后提醒',
        prompt: '这是真实投递',
        at: new Date(Date.now() + 3000).toISOString(),
      },
    })
    const dueId = due.result.record.id
    let delivered
    for (let attempt = 0; attempt < 60; attempt += 1) {
      const current = await host.call('nano.schedule.catalog', { address: addr })
      delivered = current.result.entries.find((entry) => entry.id === dueId)
      if (delivered?.status === 'inactive' && delivered?.lastDelivery) break
      await new Promise((resolve) => setTimeout(resolve, 1000))
    }
    assert.equal(delivered.status, 'inactive')
    assert.ok(delivered.lastDelivery.deliveredAt)
    assert.equal(delivered.lastDelivery.scheduledAt, due.result.record.scheduledAt)
    const deliveryHistory = await host.call('nano.schedule.history', {
      address: addr,
      id: dueId,
      limit: 10,
    })
    assert.equal(deliveryHistory.result.result.records.length, 1)
    assert.equal(deliveryHistory.result.result.records[0].messageId, delivered.lastDelivery.messageId)
    const activeAfterOneShot = await host.call('nano.schedule.list', { address: addr })
    assert.ok(
      !activeAfterOneShot.result.records.some((record) => record.id === dueId),
      'list must exclude an inactive one-shot task',
    )
    // A recurring task missed while the host is down gets exactly its latest
    // occurrence. The interval floor is 60 seconds; waiting more than two
    // intervals proves the host does not replay every missed occurrence.
    const recurring = await host.call('nano.schedule.create', {
      address: addr,
      request: { title: '周期补投', prompt: '周期提醒', every_seconds: 60 },
    })
    const recurringId = recurring.result.record.id
    // A one-shot whose target passes while the host is down must still be
    // delivered after the restart.
    const overdue = await host.call('nano.schedule.create', {
      address: addr,
      request: {
        title: '停机过期的提醒',
        prompt: '停机后补投',
        at: new Date(Date.now() + 4000).toISOString(),
      },
    })
    const overdueId = overdue.result.record.id
    const exitCode = await host.stop()
    assert.equal(exitCode, 0, `host did not exit cleanly; stderr=${host.stderr.slice(-2000)}`)

    await new Promise((resolve) => setTimeout(resolve, 130_000))

    const resumePatch = writePatch({
      root,
      dataRoot,
      sessionId,
      runtimeId: `rt-${randomUUID()}`,
      work,
      mode: 'resume',
    })
    const resumed = await boot({ dshHome, patchPath: resumePatch, work })
    host = resumed.host
    assert.equal(resumed.hello.method, 'nano.hello', `startup error: ${JSON.stringify(resumed.hello)}`)

    let stillActive
    let overdueAfter
    for (let attempt = 0; attempt < 60; attempt += 1) {
      const afterResume = await host.call('nano.schedule.catalog', { address: addr })
      stillActive = afterResume.result.entries.find((entry) => entry.id === recurringId)
      overdueAfter = afterResume.result.entries.find((entry) => entry.id === overdueId)
      if (stillActive?.lastDelivery && overdueAfter?.lastDelivery) break
      await new Promise((resolve) => setTimeout(resolve, 1000))
    }
    assert.ok(stillActive.lastDelivery, 'the missed occurrence is delivered after restart')
    assert.equal(stillActive.status, 'active')
    assert.ok(overdueAfter.lastDelivery, 'an expired one-shot is still delivered after restart')
    assert.equal(overdueAfter.status, 'inactive')
    const everyHistory = await host.call('nano.schedule.history', {
      address: addr,
      id: recurringId,
      limit: 10,
    })
    assert.equal(everyHistory.result.result.records.length, 1, 'only the latest missed occurrence is delivered')
    const overdueHistory = await host.call('nano.schedule.history', {
      address: addr,
      id: overdueId,
      limit: 10,
    })
    assert.equal(overdueHistory.result.result.records.length, 1)

    const resumeExit = await host.stop()
    assert.equal(resumeExit, 0)
  },
)

test(
  'a live schedule lease refuses startup before the scheduler mounts',
  { skip: process.env.NANO_E2E !== '1' ? 'set NANO_E2E=1' : false, timeout: 120_000 },
  async (t) => {
    const root = tempRoot()
    const dshHome = path.join(root, 'dsh-home')
    const dataRoot = path.join(root, 'data')
    const work = path.join(root, 'work')
    fs.mkdirSync(work, { recursive: true })
    installProfile(dshHome, dataRoot)
    const sessionId = `session-${randomUUID()}`
    const lockFile = path.join(dataRoot, 'runtime-data', sessionId, 'schedule.lock')
    fs.mkdirSync(path.dirname(lockFile), { recursive: true, mode: 0o700 })
    fs.writeFileSync(
      lockFile,
      JSON.stringify({ version: 1, sessionId, hostPid: process.pid, acquiredAt: new Date().toISOString() }),
      { mode: 0o600 },
    )
    const patch = writePatch({ root, dataRoot, sessionId, runtimeId: `rt-${randomUUID()}`, work })
    const host = startHost({ dshHome, patchPath: patch, work })
    t.after(() => {
      fs.rmSync(root, { recursive: true, force: true })
    })
    const frame = await host.waitFor(
      (candidate) => candidate.method === 'nano.hello' || candidate.method === 'nano.startupError',
      90_000,
    )
    assert.equal(frame.method, 'nano.startupError')
    assert.equal(frame.params.code, 'occupied')
    assert.equal(frame.params.holderPid, process.pid)
    assert.equal(fs.existsSync(lockFile), true, 'a live lock is never deleted')
    if (host.child.exitCode === null) host.child.kill('SIGKILL')
  },
)

test(
  'a wrong schedule route refuses startup instead of falling back to json',
  { skip: process.env.NANO_E2E !== '1' ? 'set NANO_E2E=1' : false, timeout: 120_000 },
  async (t) => {
    const root = tempRoot()
    const dshHome = path.join(root, 'dsh-home')
    const dataRoot = path.join(root, 'data')
    const work = path.join(root, 'work')
    fs.mkdirSync(work, { recursive: true })
    installProfile(dshHome, dataRoot)
    const sessionId = `session-${randomUUID()}`
    const patch = writePatch({
      root,
      dataRoot,
      sessionId,
      runtimeId: `rt-${randomUUID()}`,
      work,
      route: 'json',
    })
    const host = startHost({ dshHome, patchPath: patch, work })
    t.after(() => fs.rmSync(root, { recursive: true, force: true }))
    const frame = await host.waitFor(
      (candidate) => candidate.method === 'nano.hello' || candidate.method === 'nano.startupError',
      90_000,
    )
    assert.equal(frame.method, 'nano.startupError')
    assert.equal(frame.params.code, 'unsupported')
    assert.match(frame.params.message, /routes\.schedule/)
    assert.equal(fs.existsSync(path.join(dataRoot, 'runtime-data', sessionId, 'storage', 'schedule.json')), false)
    if (host.child.exitCode === null) host.child.kill('SIGKILL')
  },
)

test(
  'an unprovided backend service key times out as unsupported and never mounts the scheduler',
  { skip: process.env.NANO_E2E !== '1' ? 'set NANO_E2E=1' : false, timeout: 120_000 },
  async (t) => {
    // A routed name whose service key is never provided makes storage-domain
    // wait forever. `--patch` cannot register a backend from outside the
    // bridge, so an unknown route name is the honest stand-in for "registered
    // but forgot ctx.provide": both leave the injected key absent.
    const root = tempRoot()
    const dshHome = path.join(root, 'dsh-home')
    const dataRoot = path.join(root, 'data')
    const work = path.join(root, 'work')
    fs.mkdirSync(work, { recursive: true })
    installProfile(dshHome, dataRoot)
    const sessionId = `session-${randomUUID()}`
    const patch = writePatch({
      root,
      dataRoot,
      sessionId,
      runtimeId: `rt-${randomUUID()}`,
      work,
      route: 'missing-backend',
    })
    const host = startHost({ dshHome, patchPath: patch, work })
    t.after(() => fs.rmSync(root, { recursive: true, force: true }))
    const frame = await host.waitFor(
      (candidate) => candidate.method === 'nano.hello' || candidate.method === 'nano.startupError',
      30_000,
    )
    assert.equal(frame.method, 'nano.startupError')
    assert.equal(frame.params.code, 'unsupported')
    assert.match(frame.params.message, /storageDomain/)
    assert.equal(fs.existsSync(path.join(dataRoot, 'runtime-data', sessionId, 'storage', 'schedule.json')), false)
    if (host.child.exitCode === null) host.child.kill('SIGKILL')
  },
)

test(
  'a corrupt probe leftover is replaced instead of bricking startup',
  { skip: process.env.NANO_E2E !== '1' ? 'set NANO_E2E=1' : false, timeout: 120_000 },
  async (t) => {
    const root = tempRoot()
    const dshHome = path.join(root, 'dsh-home')
    const dataRoot = path.join(root, 'data')
    const work = path.join(root, 'work')
    fs.mkdirSync(work, { recursive: true })
    installProfile(dshHome, dataRoot)
    const sessionId = `session-${randomUUID()}`
    const storage = path.join(dataRoot, 'runtime-data', sessionId, 'storage')
    fs.mkdirSync(storage, { recursive: true, mode: 0o700 })
    const probeFile = path.join(storage, 'nano_probe.json')
    fs.writeFileSync(probeFile, '{}', { mode: 0o600 })
    const patch = writePatch({ root, dataRoot, sessionId, runtimeId: `rt-${randomUUID()}`, work })
    const host = startHost({ dshHome, patchPath: patch, work })
    t.after(() => fs.rmSync(root, { recursive: true, force: true }))
    const frame = await host.waitFor(
      (candidate) => candidate.method === 'nano.hello' || candidate.method === 'nano.startupError',
      90_000,
    )
    assert.equal(frame.method, 'nano.hello', `startup error: ${JSON.stringify(frame)}`)
    assert.equal(fs.existsSync(probeFile), false, 'the repaired probe unit is removed')
    assert.equal(await host.stop(), 0)
  },
)

test(
  'two root sessions keep separate schedule tables',
  { skip: process.env.NANO_E2E !== '1' ? 'set NANO_E2E=1' : false, timeout: 180_000 },
  async (t) => {
    const root = tempRoot()
    const dshHome = path.join(root, 'dsh-home')
    const dataRoot = path.join(root, 'data')
    const workA = path.join(root, 'work-a')
    const workB = path.join(root, 'work-b')
    fs.mkdirSync(workA, { recursive: true })
    fs.mkdirSync(workB, { recursive: true })
    installProfile(dshHome, dataRoot)
    t.after(() => fs.rmSync(root, { recursive: true, force: true }))

    const sessionA = `session-${randomUUID()}`
    const sessionB = `session-${randomUUID()}`
    const patchA = writePatch({
      root: path.join(root, 'a'),
      dataRoot,
      sessionId: sessionA,
      runtimeId: `rt-${randomUUID()}`,
      work: workA,
    })
    const patchB = writePatch({
      root: path.join(root, 'b'),
      dataRoot,
      sessionId: sessionB,
      runtimeId: `rt-${randomUUID()}`,
      work: workB,
    })

    const hostA = startHost({ dshHome, patchPath: patchA, work: workA })
    const helloA = await hostA.waitFor(
      (frame) => frame.method === 'nano.hello' || frame.method === 'nano.startupError',
      90_000,
    )
    assert.equal(helloA.method, 'nano.hello', JSON.stringify(helloA))
    const createdA = await hostA.call('nano.schedule.create', {
      address: { rootSessionId: sessionA },
      request: { title: 'A 的任务', prompt: 'A', after_seconds: 3600 },
    })
    assert.ok(createdA.result.record.id)
    assert.equal(await hostA.stop(), 0)

    const hostB = startHost({ dshHome, patchPath: patchB, work: workB })
    const helloB = await hostB.waitFor(
      (frame) => frame.method === 'nano.hello' || frame.method === 'nano.startupError',
      90_000,
    )
    assert.equal(helloB.method, 'nano.hello', JSON.stringify(helloB))
    const catalogB = await hostB.call('nano.schedule.catalog', {
      address: { rootSessionId: sessionB },
    })
    assert.deepEqual(catalogB.result.entries, [])
    await hostB.call('nano.schedule.create', {
      address: { rootSessionId: sessionB },
      request: { title: 'B 的任务', prompt: 'B', after_seconds: 3600 },
    })
    assert.equal(await hostB.stop(), 0)

    const resumeA = writePatch({
      root: path.join(root, 'a-resume'),
      dataRoot,
      sessionId: sessionA,
      runtimeId: `rt-${randomUUID()}`,
      work: workA,
      mode: 'resume',
    })
    const hostA2 = startHost({ dshHome, patchPath: resumeA, work: workA })
    const helloA2 = await hostA2.waitFor(
      (frame) => frame.method === 'nano.hello' || frame.method === 'nano.startupError',
      90_000,
    )
    assert.equal(helloA2.method, 'nano.hello', JSON.stringify(helloA2))
    const catalogA = await hostA2.call('nano.schedule.catalog', {
      address: { rootSessionId: sessionA },
    })
    assert.equal(catalogA.result.entries.length, 1)
    assert.equal(catalogA.result.entries[0].title, 'A 的任务')
    assert.equal(await hostA2.stop(), 0)

    const fileA = path.join(dataRoot, 'runtime-data', sessionA, 'storage', 'schedule.json')
    const fileB = path.join(dataRoot, 'runtime-data', sessionB, 'storage', 'schedule.json')
    assert.ok(fs.existsSync(fileA))
    assert.ok(fs.existsSync(fileB))
    assert.match(fs.readFileSync(fileA, 'utf8'), /A 的任务/)
    assert.doesNotMatch(fs.readFileSync(fileA, 'utf8'), /B 的任务/)
    assert.match(fs.readFileSync(fileB, 'utf8'), /B 的任务/)
    assert.doesNotMatch(fs.readFileSync(fileB, 'utf8'), /A 的任务/)
  },
)

