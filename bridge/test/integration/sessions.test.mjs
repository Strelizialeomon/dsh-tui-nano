// Real-host S07 test: official session list, active-only rename, the
// projection-only context read, the official /compact command, the ledger's
// same-session rejection, and a persisted title after resume.
//
// Runs only with NANO_E2E=1 because it installs a profile and starts dsh.
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

function countSessionLogs(dir) {
  let count = 0
  if (!fs.existsSync(dir)) return count
  for (const entry of fs.readdirSync(dir, { withFileTypes: true })) {
    const child = path.join(dir, entry.name)
    if (entry.isDirectory()) count += countSessionLogs(child)
    else if (entry.name.endsWith('.jsonl')) count += 1
  }
  return count
}

function startHost({ dsh, dshHome, dataRoot, work, sessionId, mode, generation, nonce, patchName }) {
  const runtimeId = `rt-sessions-${randomUUID()}`
  const patchPath = path.join(dataRoot, 'runtime', patchName)
  fs.mkdirSync(path.dirname(patchPath), { recursive: true, mode: 0o700 })
  fs.writeFileSync(
    patchPath,
    JSON.stringify([
      { id: 'session-persistence-jsonl', config: { root: path.join(dataRoot, 'sessions') } },
      {
        id: 'storage-json',
        config: { root: path.join(dataRoot, 'runtime-data', sessionId, 'storage') },
      },
      {
        id: 'nano-bridge',
        config: {
          runtimeId,
          generation,
          rootSessionId: sessionId,
          cwd: work,
          dataRoot,
          sessionMode: mode,
          officialVersion: '0.2.1-rc.2',
          nonce,
          askUserMode: 'blocking',
        },
      },
    ]),
    { mode: 0o600 },
  )
  const child = spawn(process.execPath, [dsh, '--profile', 'dsh-tui-nano', '--patch', patchPath], {
    cwd: work,
    env: { ...process.env, DSH_HOME: dshHome },
    stdio: ['ignore', 'pipe', 'pipe', 'pipe', 'pipe'],
  })
  let stderr = ''
  child.stderr.on('data', (chunk) => {
    stderr += chunk.toString()
  })
  const input = child.stdio[3]
  const output = child.stdio[4]
  const frames = []
  const waiters = []
  let buffer = Buffer.alloc(0)
  output.on('data', (chunk) => {
    buffer = Buffer.concat([buffer, chunk])
    while (true) {
      const index = buffer.indexOf(0x0a)
      if (index < 0) break
      const line = buffer.subarray(0, index).toString('utf8')
      buffer = buffer.subarray(index + 1)
      if (line.trim() === '') continue
      const frame = JSON.parse(line)
      frames.push(frame)
      for (const waiter of [...waiters]) {
        if (waiter.match(frame)) {
          waiters.splice(waiters.indexOf(waiter), 1)
          clearTimeout(waiter.timer)
          waiter.resolve(frame)
        }
      }
    }
  })
  const waitFor = (match, timeout = 60_000) =>
    new Promise((resolve, reject) => {
      const existing = frames.find(match)
      if (existing !== undefined) return resolve(existing)
      const waiter = { match, resolve }
      waiter.timer = setTimeout(() => {
        waiters.splice(waiters.indexOf(waiter), 1)
        reject(
          new Error(
            `timed out waiting for frame; recent=${JSON.stringify(frames.slice(-8))}; stderr=${stderr.slice(-2000)}`,
          ),
        )
      }, timeout)
      waiters.push(waiter)
    })
  let counter = 0
  const call = async (method, params, timeout) => {
    const id = `req-${++counter}`
    input.write(`${JSON.stringify({ jsonrpc: '2.0', id, method, params })}\n`)
    return waitFor((frame) => frame.id === id, timeout)
  }
  const exit = () =>
    new Promise((resolve) => {
      if (child.exitCode !== null) return resolve(child.exitCode)
      const timer = setTimeout(() => resolve('timeout'), 15_000)
      child.once('exit', (code) => {
        clearTimeout(timer)
        resolve(code)
      })
    })
  return { child, call, waitFor, exit, stderr: () => stderr }
}

async function installProfile(root) {
  const dshHome = path.join(root, 'dsh-home')
  const dataRoot = path.join(root, 'data')
  // macOS /var is a symlink to /private/var; the host records the resolved cwd.
  const work = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), 'nano-work-')))
  const dsh = execFileSync('which', ['dsh'], { encoding: 'utf8' }).trim()
  const install = execFileSync(
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
      dsh,
      '--json',
    ],
    { encoding: 'utf8' },
  )
  assert.equal(JSON.parse(install.trim()).installed, true)
  return { dshHome, dataRoot, work, dsh }
}

test(
  'official sessions list, rename, context, compact, occupancy, and resume',
  { skip: process.env.NANO_E2E !== '1' ? 'set NANO_E2E=1' : false, timeout: 180_000 },
  async (t) => {
    const root = fs.mkdtempSync(path.join(os.tmpdir(), 'nano-sessions-e2e-'))
    const { dshHome, dataRoot, work, dsh } = await installProfile(root)
    const sessionId = `session-${randomUUID()}`
    const host = startHost({
      dsh,
      dshHome,
      dataRoot,
      work,
      sessionId,
      mode: 'create',
      generation: 1,
      nonce: 'sessions-e2e-1',
      patchName: 'sessions-e2e-1.json',
    })
    t.after(() => {
      if (host.child.exitCode === null) host.child.kill('SIGKILL')
    })

    const hello = await host.waitFor(
      (frame) => frame.method === 'nano.hello' || frame.method === 'nano.startupError',
    )
    assert.equal(hello.method, 'nano.hello', `startup error: ${JSON.stringify(hello)}`)
    const address = { rootSessionId: sessionId }
    const subscribe = await host.call('nano.subscribe', {
      address,
      kinds: ['session.event', 'conversation.status', 'conversation.inbox', 'conversation.stream', 'sessions.context'],
      fromSeq: 0,
    })
    assert.ok(subscribe.result.subscriptionId)

    const list = await host.call('nano.sessions.list', {})
    const current = list.result.sessions.find((row) => row.sessionId === sessionId)
    assert.ok(current, 'the current session is listed')
    assert.equal(current.current, true)
    assert.equal(current.agentAvailable, true)
    assert.equal(current.blank, true)
    assert.equal(current.formatStatus, 'current')
    assert.equal(current.cwd, work)

    const rename = await host.call('nano.sessions.rename', {
      sessionId,
      title: '  重命名证据  ',
    })
    assert.equal(rename.result.title, '重命名证据')
    assert.ok(rename.result.seq >= 0)

    const renamedList = await host.call('nano.sessions.list', {})
    const renamed = renamedList.result.sessions.find((row) => row.sessionId === sessionId)
    assert.equal(renamed.title, '重命名证据')

    const cold = await host.call('nano.sessions.rename', {
      sessionId: 'session-not-current',
      title: 'x',
    })
    assert.equal(cold.error.code, 1030, 'cold rename is refused')

    const context = await host.call('nano.sessions.context', {})
    assert.equal(context.result.sessionId, sessionId)
    assert.equal(context.result.approximate, true)
    assert.ok(context.result.tokenUsage)
    // No provider request has run, so occupancy is unknown rather than zero.
    assert.equal(context.result.projectedTokens, undefined)
    assert.equal(context.result.contextWindow, undefined)

    const compact = await host.call('nano.sessions.compact', {})
    assert.equal(compact.result.kind, 'success')
    assert.equal(compact.result.text, 'No compactable history yet.')
    assert.ok(compact.result.commandId)

    const compactArgs = await host.call('nano.sessions.compact', { input: 'now' })
    assert.equal(compactArgs.result.kind, 'error')
    assert.equal(compactArgs.result.text, 'Usage: /compact (no arguments)')

    const noCancel = await host.call('nano.sessions.compactCancel', {})
    assert.equal(noCancel.result.cancelled, false)

    // A second host for the same root is rejected by the Nano ledger before it
    // touches the session, with the holder pid for the UI.
    const logsBeforeRejectedHost = countSessionLogs(path.join(dataRoot, 'sessions'))
    const second = startHost({
      dsh,
      dshHome,
      dataRoot,
      work,
      sessionId,
      mode: 'resume',
      generation: 2,
      nonce: 'sessions-e2e-2',
      patchName: 'sessions-e2e-2.json',
    })
    t.after(() => {
      if (second.child.exitCode === null) second.child.kill('SIGKILL')
    })
    const occupied = await second.waitFor(
      (frame) => frame.method === 'nano.hello' || frame.method === 'nano.startupError',
    )
    assert.equal(occupied.method, 'nano.startupError')
    assert.equal(occupied.params.code, 'occupied')
    assert.equal(occupied.params.holderPid, host.child.pid)
    await second.exit()
    assert.equal(
      countSessionLogs(path.join(dataRoot, 'sessions')),
      logsBeforeRejectedHost,
      'the rejected host did not create a session log',
    )

    const shutdown = await host.call('nano.shutdown', {})
    assert.equal(shutdown.result.clean, true)
    assert.equal(await host.exit(), 0, host.stderr().slice(-2000))

    // Materialize a second session in the shared sessions root. Its title is
    // written under that root's own projection cache, not this window's; a
    // later list must leave the cold facts unknown instead of opening the log.
    const otherId = `session-${randomUUID()}`
    const other = startHost({
      dsh,
      dshHome,
      dataRoot,
      work,
      sessionId: otherId,
      mode: 'create',
      generation: 2,
      nonce: 'sessions-e2e-other',
      patchName: 'sessions-e2e-other.json',
    })
    t.after(() => {
      if (other.child.exitCode === null) other.child.kill('SIGKILL')
    })
    const otherHello = await other.waitFor(
      (frame) => frame.method === 'nano.hello' || frame.method === 'nano.startupError',
    )
    assert.equal(otherHello.method, 'nano.hello', JSON.stringify(otherHello))
    const otherRename = await other.call('nano.sessions.rename', {
      sessionId: otherId,
      title: '另一个会话',
    })
    assert.equal(otherRename.result.title, '另一个会话')
    const otherShutdown = await other.call('nano.shutdown', {})
    assert.equal(otherShutdown.result.clean, true)
    assert.equal(await other.exit(), 0, other.stderr().slice(-2000))

    // Resume the same session in a new generation: the title is durable.
    const resumed = startHost({
      dsh,
      dshHome,
      dataRoot,
      work,
      sessionId,
      mode: 'resume',
      generation: 3,
      nonce: 'sessions-e2e-3',
      patchName: 'sessions-e2e-3.json',
    })
    t.after(() => {
      if (resumed.child.exitCode === null) resumed.child.kill('SIGKILL')
    })
    const resumedHello = await resumed.waitFor(
      (frame) => frame.method === 'nano.hello' || frame.method === 'nano.startupError',
    )
    assert.equal(resumedHello.method, 'nano.hello', JSON.stringify(resumedHello))
    assert.equal(resumedHello.params.session.mode, 'resumed')
    const resumedList = await resumed.call('nano.sessions.list', {})
    const persisted = resumedList.result.sessions.find((row) => row.sessionId === sessionId)
    assert.equal(persisted.title, '重命名证据')
    assert.equal(persisted.current, true)
    const coldRow = resumedList.result.sessions.find((row) => row.sessionId === otherId)
    assert.ok(coldRow, 'the second session is listed without opening it')
    assert.equal(coldRow.current, false)
    assert.equal(coldRow.cwd, work)
    assert.equal(coldRow.title, null, 'a cold title is not read from this root cache')
    assert.equal(coldRow.updatedAt, undefined, 'cold activity stays unknown')
    // A real cold session is refused without resuming it or taking its writer.
    const coldRename = await resumed.call('nano.sessions.rename', {
      sessionId: otherId,
      title: '不能改',
    })
    assert.equal(coldRename.error.code, 1030)
    const resumedShutdown = await resumed.call('nano.shutdown', {})
    assert.equal(resumedShutdown.result.clean, true)
    assert.equal(await resumed.exit(), 0, resumed.stderr().slice(-2000))

    // A missing resume target is an explicit not-found, never an empty session.
    const missingId = `session-missing-${randomUUID()}`
    const missing = startHost({
      dsh,
      dshHome,
      dataRoot,
      work,
      sessionId: missingId,
      mode: 'resume',
      generation: 4,
      nonce: 'sessions-e2e-4',
      patchName: 'sessions-e2e-4.json',
    })
    t.after(() => {
      if (missing.child.exitCode === null) missing.child.kill('SIGKILL')
    })
    const missingHello = await missing.waitFor(
      (frame) => frame.method === 'nano.hello' || frame.method === 'nano.startupError',
    )
    assert.equal(missingHello.method, 'nano.startupError')
    assert.equal(missingHello.params.code, 'startup-failed')
    assert.match(missingHello.params.message, /session\/not-found/)
    await missing.exit()
  },
)
