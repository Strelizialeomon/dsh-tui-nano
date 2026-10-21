// Real-host Node integration test: malformed JSON is reported without killing
// the host, subscription replay works, and shutdown is clean.
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

test(
  'malformed frame is reported, subscription replays, shutdown is clean',
  { skip: process.env.NANO_E2E !== '1' ? 'set NANO_E2E=1' : false, timeout: 120_000 },
  async (t) => {
    const root = fs.mkdtempSync(path.join(os.tmpdir(), 'nano-node-e2e-'))
    const dshHome = path.join(root, 'dsh-home')
    const dataRoot = path.join(root, 'data')
    const work = path.join(root, 'work')
    fs.mkdirSync(work, { recursive: true })
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

    const sessionId = `session-${randomUUID()}`
    const runtimeId = `rt-node-${randomUUID()}`
    const nonce = 'node-e2e-nonce'
    const patchPath = path.join(dataRoot, 'runtime', 'node-e2e.json')
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
            generation: 1,
            rootSessionId: sessionId,
            cwd: work,
            dataRoot,
            sessionMode: 'create',
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
    t.after(() => {
      if (child.exitCode === null) child.kill('SIGKILL')
    })

    const waitFor = (match, timeout = 60_000) =>
      new Promise((resolve, reject) => {
        const existing = frames.find(match)
        if (existing !== undefined) return resolve(existing)
        const waiter = { match, resolve }
        waiter.timer = setTimeout(() => {
          waiters.splice(waiters.indexOf(waiter), 1)
          reject(new Error(`timed out waiting for frame; stderr=${stderr.slice(-2000)}`))
        }, timeout)
        waiters.push(waiter)
      })

    let counter = 0
    const send = (method, params) => {
      const id = `req-${++counter}`
      input.write(`${JSON.stringify({ jsonrpc: '2.0', id, method, params })}\n`)
      return id
    }

    const hello = await waitFor((frame) => frame.method === 'nano.hello' || frame.method === 'nano.startupError')
    assert.equal(hello.method, 'nano.hello', `startup error: ${JSON.stringify(hello)}`)
    assert.equal(hello.params.rootSessionId, sessionId)

    // A malformed line must produce a parse-error response with a null id and
    // must not kill the connection.
    input.write('this is not json\n')
    const parseError = await waitFor((frame) => frame.id === null && frame.error !== undefined)
    assert.equal(parseError.error.code, -32700)

    const subscribeId = send('nano.subscribe', {
      address: { rootSessionId: sessionId },
      fromSeq: 0,
    })
    const subscribeReply = await waitFor((frame) => frame.id === subscribeId)
    assert.ok(subscribeReply.result.subscriptionId)
    assert.ok(subscribeReply.result.seq >= 3)
    const firstEvent = await waitFor(
      (frame) =>
        frame.method === 'nano.event' &&
        frame.params.subscriptionId === subscribeReply.result.subscriptionId,
    )
    assert.equal(firstEvent.params.kind, 'session.event')
    assert.equal(firstEvent.params.generation, 1)

    const shutdownId = send('nano.shutdown', {})
    const shutdownReply = await waitFor((frame) => frame.id === shutdownId)
    assert.equal(shutdownReply.result.clean, true)
    assert.ok(shutdownReply.result.steps.every((step) => step.status === 'stopped'))

    const exitCode = await new Promise((resolve) => {
      if (child.exitCode !== null) return resolve(child.exitCode)
      const timer = setTimeout(() => resolve('timeout'), 15_000)
      child.once('exit', (code) => {
        clearTimeout(timer)
        resolve(code)
      })
    })
    assert.equal(exitCode, 0, `host did not exit cleanly; stderr=${stderr.slice(-2000)}`)

    const ledger = JSON.parse(fs.readFileSync(path.join(dataRoot, 'session-mounts.json'), 'utf8'))
    assert.deepEqual(ledger.owners, [])
  },
)
