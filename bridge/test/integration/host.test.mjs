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
          id: 'storage-domain',
          config: {
            backend: 'json',
            routes: { schedule: 'nano-schedule-json', nano_probe: 'nano-schedule-json' },
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
    let step = 'hello'
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
        if (process.env.NANO_DEBUG === '1') console.error('FRAME', JSON.stringify(frame))
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
          reject(new Error(`timed out waiting for frame (${step}); recent=${JSON.stringify(frames.slice(-8))}; stderr=${stderr.slice(-2000)}`))
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

test(
  'conversation surface submits, dedupes, and keeps official identity',
  { skip: process.env.NANO_E2E !== '1' ? 'set NANO_E2E=1' : false, timeout: 180_000 },
  async (t) => {
    const root = fs.mkdtempSync(path.join(os.tmpdir(), 'nano-conv-e2e-'))
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
    const runtimeId = `rt-conv-${randomUUID()}`
    const nonce = 'conv-e2e-nonce'
    const patchPath = path.join(dataRoot, 'runtime', 'conv-e2e.json')
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
          id: 'storage-domain',
          config: {
            backend: 'json',
            routes: { schedule: 'nano-schedule-json', nano_probe: 'nano-schedule-json' },
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
    let step = 'hello'
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
        if (process.env.NANO_DEBUG === '1') console.error('FRAME', JSON.stringify(frame))
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
          reject(new Error(`timed out waiting for frame (${step}); recent=${JSON.stringify(frames.slice(-8))}; stderr=${stderr.slice(-2000)}`))
        }, timeout)
        waiters.push(waiter)
      })

    let counter = 0
    const send = (method, params) => {
      const id = `req-${++counter}`
      input.write(`${JSON.stringify({ jsonrpc: '2.0', id, method, params })}\n`)
      return id
    }
    const call = async (method, params) => {
      step = method
      const id = send(method, params)
      const reply = await waitFor((frame) => frame.id === id)
      return reply
    }

    const hello = await waitFor((frame) => frame.method === 'nano.hello' || frame.method === 'nano.startupError')
    assert.equal(hello.method, 'nano.hello', `startup error: ${JSON.stringify(hello)}`)
    const address = { rootSessionId: sessionId }
    const subscribe = await call('nano.subscribe', {
      address,
      kinds: ['session.event', 'conversation.status', 'conversation.inbox', 'conversation.stream'],
      fromSeq: 0,
    })
    assert.ok(subscribe.result.subscriptionId)

    const snapshot = await call('nano.conversation.snapshot', { address })
    assert.equal(snapshot.result.status, 'idle')
    assert.ok(snapshot.result.seq >= 3)
    assert.deepEqual(snapshot.result.inbox, { nextTurn: [], nextStep: [] })

    const empty = await call('nano.conversation.submit', {
      address,
      requestId: 'empty-request',
      delivery: 'queue',
      content: [{ type: 'text', text: '   \n' }],
    })
    assert.equal(empty.error.code, 1012)

    const steerIdle = await call('nano.conversation.submit', {
      address,
      requestId: 'steer-idle',
      delivery: 'steer',
      content: [{ type: 'text', text: 'x' }],
    })
    assert.equal(steerIdle.error.code, 1016)

    const missingEdit = await call('nano.conversation.editQueued', {
      address,
      messageId: 'missing',
      content: [{ type: 'text', text: 'x' }],
    })
    assert.equal(missingEdit.error.code, 1014)

    const childAddress = await call('nano.conversation.snapshot', {
      address: { rootSessionId: sessionId, childSessionId: 'child' },
    })
    assert.equal(childAddress.error.code, 1008)

    const requestId = `prompt-${randomUUID()}`
    const first = await call('nano.conversation.submit', {
      address,
      requestId,
      delivery: 'queue',
      content: [{ type: 'text', text: 'hello from the conversation surface' }],
    })
    assert.equal(first.result.accepted, true)
    assert.equal(first.result.duplicate, false)
    assert.equal(first.result.requestId, requestId)
    assert.equal(typeof first.result.messageId, 'string')

    const second = await call('nano.conversation.submit', {
      address,
      requestId,
      delivery: 'queue',
      content: [{ type: 'text', text: 'a second copy must not exist' }],
    })
    assert.equal(second.result.duplicate, true)
    assert.equal(second.result.messageId, first.result.messageId)

    // Without a selected model the official loop fails before committing a
    // `user/message`, but the accepted prompt is already durable in the inbox
    // splice. That is the identity the UI must keep; a model-backed run adds
    // the `user/message` on top (S04 selects the route).
    const durable = await waitFor(
      (frame) =>
        frame.method === 'nano.event' &&
        frame.params.kind === 'session.event' &&
        frame.params.payload.type === 'agent/inbox/spliced' &&
        frame.params.payload.data?.inserted?.some(
          (message) => message.id === first.result.messageId && message.source?.rpcId === requestId,
        ),
    )
    const inserted = durable.params.payload.data.inserted.find(
      (message) => message.id === first.result.messageId,
    )
    assert.equal(inserted.source.kind, 'user')
    assert.equal(inserted.source.rpcId, requestId)

    const cancel = await call('nano.conversation.cancel', { address })
    assert.equal(cancel.result.accepted, true)

    const shutdown = await call('nano.shutdown', {})
    assert.equal(shutdown.result.clean, true)
    const exitCode = await new Promise((resolve) => {
      if (child.exitCode !== null) return resolve(child.exitCode)
      const timer = setTimeout(() => resolve('timeout'), 15_000)
      child.once('exit', (code) => {
        clearTimeout(timer)
        resolve(code)
      })
    })
    assert.equal(exitCode, 0, `host did not exit cleanly; stderr=${stderr.slice(-2000)}`)

    // Rebuild from the official log: a fresh host resumes the same session and
    // replays the durable inbox splice without any local cache.
    const resumePatchPath = path.join(dataRoot, 'runtime', 'conv-e2e-resume.json')
    fs.writeFileSync(
      resumePatchPath,
      JSON.stringify([
        { id: 'session-persistence-jsonl', config: { root: path.join(dataRoot, 'sessions') } },
        {
          id: 'storage-json',
          config: { root: path.join(dataRoot, 'runtime-data', sessionId, 'storage') },
        },
        {
          id: 'storage-domain',
          config: {
            backend: 'json',
            routes: { schedule: 'nano-schedule-json', nano_probe: 'nano-schedule-json' },
          },
        },
        {
          id: 'nano-bridge',
          config: {
            runtimeId: `rt-conv-resume-${randomUUID()}`,
            generation: 2,
            rootSessionId: sessionId,
            cwd: work,
            dataRoot,
            sessionMode: 'resume',
            officialVersion: '0.2.1-rc.2',
            nonce: 'conv-e2e-resume-nonce',
            askUserMode: 'blocking',
          },
        },
      ]),
      { mode: 0o600 },
    )
    const resumed = spawn(process.execPath, [dsh, '--profile', 'dsh-tui-nano', '--patch', resumePatchPath], {
      cwd: work,
      env: { ...process.env, DSH_HOME: dshHome },
      stdio: ['ignore', 'pipe', 'pipe', 'pipe', 'pipe'],
    })
    let resumedStderr = ''
    resumed.stderr.on('data', (chunk) => {
      resumedStderr += chunk.toString()
    })
    const resumedInput = resumed.stdio[3]
    const resumedOutput = resumed.stdio[4]
    const resumedFrames = []
    const resumedWaiters = []
    let resumedBuffer = Buffer.alloc(0)
    resumedOutput.on('data', (chunk) => {
      resumedBuffer = Buffer.concat([resumedBuffer, chunk])
      while (true) {
        const index = resumedBuffer.indexOf(0x0a)
        if (index < 0) break
        const line = resumedBuffer.subarray(0, index).toString('utf8')
        resumedBuffer = resumedBuffer.subarray(index + 1)
        if (line.trim() === '') continue
        const frame = JSON.parse(line)
        resumedFrames.push(frame)
        for (const waiter of [...resumedWaiters]) {
          if (waiter.match(frame)) {
            resumedWaiters.splice(resumedWaiters.indexOf(waiter), 1)
            clearTimeout(waiter.timer)
            waiter.resolve(frame)
          }
        }
      }
    })
    t.after(() => {
      if (resumed.exitCode === null) resumed.kill('SIGKILL')
    })
    const waitResumed = (match, timeout = 60_000) =>
      new Promise((resolve, reject) => {
        const existing = resumedFrames.find(match)
        if (existing !== undefined) return resolve(existing)
        const waiter = { match, resolve }
        waiter.timer = setTimeout(() => {
          resumedWaiters.splice(resumedWaiters.indexOf(waiter), 1)
          reject(new Error(`resume timed out; stderr=${resumedStderr.slice(-2000)}`))
        }, timeout)
        resumedWaiters.push(waiter)
      })
    let resumedCounter = 0
    const resumedSend = (method, params) => {
      const id = `resume-${++resumedCounter}`
      resumedInput.write(`${JSON.stringify({ jsonrpc: '2.0', id, method, params })}\n`)
      return id
    }
    const resumedHello = await waitResumed(
      (frame) => frame.method === 'nano.hello' || frame.method === 'nano.startupError',
    )
    assert.equal(resumedHello.method, 'nano.hello', `resume startup error: ${JSON.stringify(resumedHello)}`)
    assert.equal(resumedHello.params.session.mode, 'resumed')
    const resumedSubscribeId = resumedSend('nano.subscribe', {
      address,
      kinds: ['session.event', 'conversation.status', 'conversation.inbox', 'conversation.stream'],
      fromSeq: 0,
    })
    await waitResumed((frame) => frame.id === resumedSubscribeId)
    const replayed = await waitResumed(
      (frame) =>
        frame.method === 'nano.event' &&
        frame.params.kind === 'session.event' &&
        frame.params.payload.type === 'agent/inbox/spliced' &&
        frame.params.payload.data?.inserted?.some((message) => message.id === first.result.messageId),
    )
    assert.equal(replayed.params.generation, 2)
    const resumedShutdownId = resumedSend('nano.shutdown', {})
    const resumedShutdown = await waitResumed((frame) => frame.id === resumedShutdownId)
    assert.equal(resumedShutdown.result.clean, true)
    const resumedExit = await new Promise((resolve) => {
      if (resumed.exitCode !== null) return resolve(resumed.exitCode)
      const timer = setTimeout(() => resolve('timeout'), 15_000)
      resumed.once('exit', (code) => {
        clearTimeout(timer)
        resolve(code)
      })
    })
    assert.equal(resumedExit, 0, `resumed host did not exit cleanly; stderr=${resumedStderr.slice(-2000)}`)
  },
)
