// S13 cross-feature acceptance against the real official host.
//
// Covers the release combinations that unit suites cannot prove:
// - two windows claiming the same root session;
// - lock release after a clean shutdown and re-claim by a new window;
// - durable transcript and schedule isolation between two root sessions;
// - the official schedule table living in each root's own storage partition;
// - a wrong schedule route being refused instead of falling back to json.
//
// Runs only with NANO_E2E=1 because it installs a profile and starts dsh.
import test from 'node:test'
import assert from 'node:assert/strict'
import fs from 'node:fs'
import path from 'node:path'
import { randomUUID } from 'node:crypto'

import {
  address,
  boot,
  fileContains,
  installProfile,
  sessionLogs,
  sleep,
  tempRoot,
  writePatch,
} from './harness.mjs'
import { startMockModel } from './mock-model.mjs'

const skip = process.env.NANO_E2E !== '1' ? 'set NANO_E2E=1' : false

function setup(root, t) {
  const home = path.join(root, 'home')
  const dshHome = path.join(root, 'dsh-home')
  const dataRoot = path.join(root, 'data')
  const work = path.join(root, 'work')
  fs.mkdirSync(home, { recursive: true })
  fs.mkdirSync(work, { recursive: true })
  // Keep pnpm/npm caches inside the synthetic root as well.
  process.env.HOME = home
  process.env.XDG_CACHE_HOME = path.join(home, '.cache')
  process.env.XDG_DATA_HOME = path.join(home, '.local', 'share')
  process.env.XDG_STATE_HOME = path.join(home, '.local', 'state')
  installProfile({ dshHome, dataRoot })
  const hosts = []
  t.after(() => {
    for (const host of hosts) {
      if (host.child.exitCode === null) host.child.kill('SIGKILL')
    }
    if (process.env.NANO_KEEP !== '1') fs.rmSync(root, { recursive: true, force: true })
  })
  return { dshHome, dataRoot, work, hosts }
}

async function start(options, hosts) {
  return await boot(options, (host) => hosts.push(host))
}

async function cleanStop(host, label) {
  const result = await host.stop()
  assert.equal(result.code, 0, `${label} exit: ${host.stderr.slice(-2000)}`)
  assert.equal(
    result.clean,
    true,
    `${label} shutdown was not clean: ${JSON.stringify(result.reply)}`,
  )
}

test(
  'same root session is rejected, then released and re-claimable',
  { skip, timeout: 300_000 },
  async (t) => {
    const root = tempRoot()
    const { dshHome, dataRoot, work, hosts } = setup(root, t)

    const sessionId = `session-${randomUUID()}`
    const first = await start(
      {
        dshHome,
        work,
        patchPath: writePatch({
          root,
          dataRoot,
          sessionId,
          runtimeId: `rt-${randomUUID()}`,
          work,
          mode: 'create',
          generation: 1,
        }),
      },
      hosts,
    )
    assert.equal(first.hello.method, 'nano.hello', JSON.stringify(first.hello))
    const logCountBefore = sessionLogs(dataRoot).length

    // The second window claims the same root before any schedule mount.
    const second = await start(
      {
        dshHome,
        work,
        patchPath: writePatch({
          root,
          dataRoot,
          sessionId,
          runtimeId: `rt-${randomUUID()}`,
          work,
          mode: 'resume',
          generation: 2,
        }),
      },
      hosts,
    )
    assert.equal(second.hello.method, 'nano.startupError', JSON.stringify(second.hello))
    assert.equal(second.hello.params.code, 'occupied')
    assert.equal(second.hello.params.holderPid, first.host.child.pid)
    const secondExit = await second.host.stop()
    assert.ok([0, 1].includes(secondExit.code), `rejected host exit ${secondExit.code}`)
    assert.equal(
      sessionLogs(dataRoot).length,
      logCountBefore,
      'a rejected host must not create a session log',
    )

    await cleanStop(first.host, 'first')

    // After the clean shutdown the ledger entry is gone; a new window may claim.
    const third = await start(
      {
        dshHome,
        work,
        patchPath: writePatch({
          root,
          dataRoot,
          sessionId,
          runtimeId: `rt-${randomUUID()}`,
          work,
          mode: 'resume',
          generation: 3,
        }),
      },
      hosts,
    )
    assert.equal(third.hello.method, 'nano.hello', JSON.stringify(third.hello))
    assert.equal(third.hello.params.rootSessionId, sessionId)
    await cleanStop(third.host, 'third')
  },
)

test(
  'two live windows with different roots do not terminate each other',
  { skip, timeout: 300_000 },
  async (t) => {
    const root = tempRoot()
    const { dshHome, dataRoot, work, hosts } = setup(root, t)
    const workA = path.join(root, 'work-a')
    const workB = path.join(root, 'work-b')
    fs.mkdirSync(workA, { recursive: true })
    fs.mkdirSync(workB, { recursive: true })
    const sessionA = `session-${randomUUID()}`
    const sessionB = `session-${randomUUID()}`

    const [a, b] = await Promise.all([
      start(
        {
          dshHome,
          work: workA,
          patchPath: writePatch({
            root,
            dataRoot,
            sessionId: sessionA,
            runtimeId: `rt-${randomUUID()}`,
            work: workA,
            mode: 'create',
            generation: 1,
          }),
        },
        hosts,
      ),
      start(
        {
          dshHome,
          work: workB,
          patchPath: writePatch({
            root,
            dataRoot,
            sessionId: sessionB,
            runtimeId: `rt-${randomUUID()}`,
            work: workB,
            mode: 'create',
            generation: 1,
          }),
        },
        hosts,
      ),
    ])
    assert.equal(a.hello.method, 'nano.hello', JSON.stringify(a.hello))
    assert.equal(b.hello.method, 'nano.hello', JSON.stringify(b.hello))
    assert.notEqual(a.hello.params.rootSessionId, b.hello.params.rootSessionId)

    const createdA = await a.host.call('nano.schedule.create', {
      address: address(sessionA),
      request: { title: '窗口 A', prompt: 'A', after_seconds: 3600 },
    })
    const createdB = await b.host.call('nano.schedule.create', {
      address: address(sessionB),
      request: { title: '窗口 B', prompt: 'B', after_seconds: 3600 },
    })
    assert.ok(createdA.result.record.id, JSON.stringify(createdA))
    assert.ok(createdB.result.record.id, JSON.stringify(createdB))

    // Closing window A must leave window B alive and serving its own root.
    await cleanStop(a.host, 'window-a')
    const listB = await b.host.call('nano.schedule.list', {
      address: address(sessionB),
    })
    assert.deepEqual(
      listB.result.records.map((record) => record.title),
      ['窗口 B'],
    )
    await cleanStop(b.host, 'window-b')

    for (const sessionId of [sessionA, sessionB]) {
      assert.ok(
        fs.existsSync(path.join(dataRoot, 'runtime-data', sessionId, 'storage')),
        `missing storage partition for ${sessionId}`,
      )
    }
  },
)

test(
  'transcripts and schedule tables do not cross root sessions',
  { skip, timeout: 300_000 },
  async (t) => {
    const root = tempRoot()
    const { dshHome, dataRoot, work, hosts } = setup(root, t)
    const mock = await startMockModel()
    t.after(() => mock.close())
    const env = {
      DEEPSEEK_API_KEY: 'nano-s13-test-key',
      DEEPSEEK_BASE_URL: mock.url,
    }

    const sessionA = `session-${randomUUID()}`
    const sessionB = `session-${randomUUID()}`
    const markerA = `S13MARK-A-${randomUUID()}`
    const markerB = `S13MARK-B-${randomUUID()}`

    const runRoot = async ({ sessionId, marker, title, generation }) => {
      const host = await start(
        {
          dshHome,
          work,
          env,
          patchPath: writePatch({
            root,
            dataRoot,
            sessionId,
            runtimeId: `rt-${randomUUID()}`,
            work,
            mode: generation === 1 ? 'create' : 'resume',
            generation,
          }),
        },
        hosts,
      )
      assert.equal(host.hello.method, 'nano.hello', JSON.stringify(host.hello))
      // A real submit writes the marker into the durable session event stream;
      // the local mock only serves the model response.
      const subscription = await host.host.call('nano.subscribe', {
        address: address(sessionId),
        fromSeq: 0,
      })
      assert.ok(subscription.result.subscriptionId, JSON.stringify(subscription))
      const submitted = await host.host.call('nano.conversation.submit', {
        address: address(sessionId),
        requestId: `s13-${randomUUID()}`,
        delivery: 'queue',
        content: [{ type: 'text', text: marker }],
      })
      assert.equal(submitted.result.accepted, true, JSON.stringify(submitted))
      await host.host.waitFor(
        (frame) =>
          frame.method === 'nano.event' &&
          frame.params.subscriptionId === subscription.result.subscriptionId &&
          frame.params.kind === 'session.event' &&
          frame.params.payload?.type === 'user/message' &&
          JSON.stringify(frame.params.payload).includes(marker),
        30_000,
      )
      await sleep(500)
      const created = await host.host.call('nano.schedule.create', {
        address: address(sessionId),
        request: { title, prompt: title, after_seconds: 3600 },
      })
      assert.ok(created.result.record.id, JSON.stringify(created))
      await sleep(500)
      await cleanStop(host.host, sessionId)
    }

    await runRoot({ sessionId: sessionA, marker: markerA, title: '任务 A', generation: 1 })
    await runRoot({ sessionId: sessionB, marker: markerB, title: '任务 B', generation: 1 })
    const logs = sessionLogs(dataRoot)
    assert.ok(mock.requests() >= 2, 'the real conversation path did not reach the local model')
    for (const [sessionId, marker] of [
      [sessionA, markerA],
      [sessionB, markerB],
    ]) {
      const markerFiles = logs.filter((file) => fileContains(file, marker))
      assert.ok(
        markerFiles.some((file) => fileContains(file, 'S13-MOCK-OK')),
        `root ${sessionId} has no durable assistant response`,
      )
    }

    // Durable transcripts: each marker appears only in its own root's log.
    const withA = logs.filter((file) => fileContains(file, markerA))
    const withB = logs.filter((file) => fileContains(file, markerB))
    assert.ok(withA.length > 0, `no durable transcript contains ${markerA}`)
    assert.ok(withB.length > 0, `no durable transcript contains ${markerB}`)
    assert.deepEqual(
      withA.filter((file) => fileContains(file, markerB)),
      [],
      'marker B leaked into root A transcript',
    )
    assert.deepEqual(
      withB.filter((file) => fileContains(file, markerA)),
      [],
      'marker A leaked into root B transcript',
    )
    assert.notDeepEqual(withA, withB, 'both roots wrote the same session log')

    // Every root has its own storage partition.
    for (const sessionId of [sessionA, sessionB]) {
      const storage = path.join(dataRoot, 'runtime-data', sessionId, 'storage')
      assert.ok(fs.existsSync(storage), `missing storage partition: ${storage}`)
    }

    // Each root's official schedule table only contains its own record.
    const listFor = async (sessionId, generation) => {
      const host = await start(
        {
          dshHome,
          work,
          env,
          patchPath: writePatch({
            root,
            dataRoot,
            sessionId,
            runtimeId: `rt-${randomUUID()}`,
            work,
            mode: 'resume',
            generation,
          }),
        },
        hosts,
      )
      assert.equal(host.hello.method, 'nano.hello', JSON.stringify(host.hello))
      const list = await host.host.call('nano.schedule.list', {
        address: address(sessionId),
      })
      await cleanStop(host.host, `${sessionId}-list`)
      return list.result.records.map((record) => record.title)
    }

    const titlesA = await listFor(sessionA, 2)
    const titlesB = await listFor(sessionB, 2)
    assert.deepEqual(titlesA, ['任务 A'])
    assert.deepEqual(titlesB, ['任务 B'])
  },
)

test(
  'official schedule route is nano-schedule-json and rejects another route',
  { skip, timeout: 300_000 },
  async (t) => {
    const root = tempRoot()
    const { dshHome, dataRoot, work, hosts } = setup(root, t)

    const sessionId = `session-${randomUUID()}`
    const good = await start(
      {
        dshHome,
        work,
        patchPath: writePatch({
          root,
          dataRoot,
          sessionId,
          runtimeId: `rt-${randomUUID()}`,
          work,
          mode: 'create',
          generation: 1,
        }),
      },
      hosts,
    )
    assert.equal(good.hello.method, 'nano.hello', JSON.stringify(good.hello))
    assert.equal(good.hello.params.capabilities.schedule.status, 'supported')
    const catalog = await good.host.call('nano.schedule.catalog', {
      address: address(sessionId),
    })
    assert.deepEqual(catalog.result.entries, [])
    await cleanStop(good.host, 'route-good')

    const otherSession = `session-${randomUUID()}`
    const bad = await start(
      {
        dshHome,
        work,
        patchPath: writePatch({
          root,
          dataRoot,
          sessionId: otherSession,
          runtimeId: `rt-${randomUUID()}`,
          work,
          mode: 'create',
          generation: 2,
          route: 'json',
        }),
      },
      hosts,
    )
    assert.equal(bad.hello.method, 'nano.startupError', JSON.stringify(bad.hello))
    assert.equal(bad.hello.params.code, 'unsupported')
    // Depending on which activation fails first, the official message names
    // `routes.schedule` or the storage-domain timeout. Both are refusals.
    assert.match(bad.hello.params.message, /schedule|storageDomain/)
    assert.equal(
      fs.existsSync(path.join(dataRoot, 'runtime-data', otherSession, 'storage', 'schedule.json')),
      false,
    )
    const exit = await bad.host.stop()
    assert.ok([0, 1].includes(exit.code), `rejected host exit ${exit.code}`)
  },
)
