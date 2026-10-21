// S05 follow-up real-host evidence: nested instructions discovered through a
// real first-party read tool call, a changed file reconciled after --resume,
// and both failure modes (root-marker probe, unreadable file) without a
// fabricated instruction set.
//
// Runs only with NANO_E2E=1. The DeepSeek route points at a local Messages
// mock, so the real pre-step and tools run without any model spend. Requests
// are classified by body so the session-title auxiliary request is never
// mistaken for the model's post-tool answer.
import test from 'node:test'
import assert from 'node:assert/strict'
import fs from 'node:fs'
import os from 'node:os'
import path from 'node:path'
import http from 'node:http'
import { spawn, execFileSync } from 'node:child_process'
import { randomUUID } from 'node:crypto'
import { fileURLToPath } from 'node:url'

const bridgeRoot = fileURLToPath(new URL('../..', import.meta.url))
const installScript = path.join(bridgeRoot, 'profile', 'install.mjs')
const isRoot = typeof process.getuid === 'function' && process.getuid() === 0

function installProfile(dshHome, dataRoot) {
  const dsh = execFileSync('which', ['dsh'], { encoding: 'utf8' }).trim()
  const output = execFileSync(
    process.execPath,
    [
      installScript,
      '--dsh-home', dshHome,
      '--data-root', dataRoot,
      '--bridge-package', bridgeRoot,
      '--dsh-bin', dsh,
      '--json',
    ],
    { encoding: 'utf8' },
  )
  assert.equal(JSON.parse(output.trim()).installed, true)
}

function sseEvents(events) {
  return events.map((event) => `event: ${event.type}\ndata: ${JSON.stringify(event)}\n\n`).join('')
}

function textResponse(res, text = 'ok') {
  res.writeHead(200, { 'content-type': 'text/event-stream' })
  res.end(
    sseEvents([
      { type: 'message_start', message: { usage: { input_tokens: 1, output_tokens: 0 } } },
      { type: 'content_block_start', index: 0, content_block: { type: 'text', text: '' } },
      { type: 'content_block_delta', index: 0, delta: { type: 'text_delta', text } },
      { type: 'content_block_stop', index: 0 },
      { type: 'message_delta', delta: { stop_reason: 'end_turn' }, usage: { output_tokens: 1 } },
      { type: 'message_stop' },
    ]),
  )
}

function toolUseResponse(res, tool, input) {
  res.writeHead(200, { 'content-type': 'text/event-stream' })
  res.end(
    sseEvents([
      { type: 'message_start', message: { usage: { input_tokens: 1, output_tokens: 0 } } },
      {
        type: 'content_block_start',
        index: 0,
        content_block: { type: 'tool_use', id: `toolu_${randomUUID()}`, name: tool, input },
      },
      { type: 'content_block_stop', index: 0 },
      { type: 'message_delta', delta: { stop_reason: 'tool_use' }, usage: { output_tokens: 1 } },
      { type: 'message_stop' },
    ]),
  )
}

function startModel(handler) {
  const requests = { agent: 0, title: 0, bodies: [] }
  const server = http.createServer((request, response) => {
    let body = ''
    request.on('data', (chunk) => {
      body += chunk
    })
    request.on('end', () => {
      requests.bodies.push(body)
      let title = false
      try {
        const first = JSON.parse(body).messages?.[0]?.content?.[0]?.text
        title = typeof first === 'string' && first.startsWith('Generate the session title')
      } catch {
        title = false
      }
      if (title) {
        requests.title += 1
        textResponse(response, 'title')
        return
      }
      requests.agent += 1
      handler(requests, response)
    })
  })
  return new Promise((resolve) => {
    server.listen(0, '127.0.0.1', () => {
      resolve({
        requests,
        port: server.address().port,
        close: () => new Promise((done) => server.close(done)),
      })
    })
  })
}

function waitForCondition(predicate, timeout, label) {
  const started = Date.now()
  return new Promise((resolve, reject) => {
    const tick = () => {
      if (predicate()) return resolve()
      if (Date.now() - started > timeout) return reject(new Error(`timed out waiting for ${label}`))
      setTimeout(tick, 25)
    }
    tick()
  })
}

function startHost({ work, dshHome, agentsHome, dataRoot, sessionId, mode, generation, nonce, modelPort }) {
  const patchPath = path.join(dataRoot, 'runtime', `s05-${generation}.json`)
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
          runtimeId: `rt-${generation}-${randomUUID()}`,
          generation,
          rootSessionId: sessionId,
          cwd: work,
          dataRoot,
          sessionMode: mode,
          officialVersion: '0.2.1-rc.2',
          nonce,
          askUserMode: 'blocking',
          provider: 'deepseek-official',
          model: 'deepseek-flash',
        },
      },
    ]),
    { mode: 0o600 },
  )
  const dsh = execFileSync('which', ['dsh'], { encoding: 'utf8' }).trim()
  const child = spawn(process.execPath, [dsh, '--profile', 'dsh-tui-nano', '--patch', patchPath], {
    cwd: work,
    env: {
      ...process.env,
      DSH_HOME: dshHome,
      DSH_AGENTS_HOME: agentsHome,
      DEEPSEEK_API_KEY: 'nano-s05-test-key',
      DEEPSEEK_BASE_URL: `http://127.0.0.1:${modelPort}`,
    },
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
      const frameIndex = frames.length - 1
      for (const waiter of [...waiters]) {
        if (waiter.match(frame, frameIndex)) {
          waiters.splice(waiters.indexOf(waiter), 1)
          clearTimeout(waiter.timer)
          waiter.resolve(frame)
        }
      }
    }
  })
  const waitFor = (match, timeout = 60_000) =>
    new Promise((resolve, reject) => {
      const existing = frames.find((frame, index) => match(frame, index))
      if (existing !== undefined) return resolve(existing)
      const waiter = { match, resolve }
      waiter.timer = setTimeout(() => {
        waiters.splice(waiters.indexOf(waiter), 1)
        reject(
          new Error(
            `timed out waiting for a frame; recent: ${JSON.stringify(
              frames.slice(-12).map((frame) => frame.params?.payload?.type ?? frame.params?.kind ?? frame.method ?? frame.id),
            )}; stderr: ${stderr.slice(-500)}`,
          ),
        )
      }, timeout)
      waiters.push(waiter)
    })
  let counter = 10
  const request = async (method, params, match) => {
    const id = `req-${generation}-${++counter}`
    input.write(`${JSON.stringify({ jsonrpc: '2.0', id, method, params })}\n`)
    return waitFor(match ?? ((frame) => frame.id === id))
  }
  const stop = async () => {
    const shutdown = await request('nano.shutdown', {})
    assert.equal(shutdown.result.clean, true)
    const exit = await new Promise((resolve, reject) => {
      if (child.exitCode !== null) return resolve(child.exitCode)
      const timer = setTimeout(() => {
        child.kill('SIGKILL')
        reject(new Error('host did not exit after shutdown'))
      }, 15_000)
      child.once('exit', (code) => {
        clearTimeout(timer)
        resolve(code)
      })
    })
    assert.equal(exit, 0)
  }
  return { child, frames, waitFor, request, stop, stderr: () => stderr }
}

function sessionEvent(frame, type) {
  return (
    frame.method === 'nano.event' &&
    frame.params.kind === 'session.event' &&
    frame.params.payload.type === type
  )
}

test(
  'nested instructions reach the next request and resume corrects a changed baseline',
  { skip: process.env.NANO_E2E !== '1' ? 'set NANO_E2E=1' : false, timeout: 240_000 },
  async (t) => {
    const root = fs.mkdtempSync(path.join(os.tmpdir(), 'nano-s05-followup-'))
    const dshHome = path.join(root, 'dsh-home')
    const agentsHome = path.join(root, 'agents-home')
    const dataRoot = path.join(root, 'data')
    const work = path.join(root, 'work')
    fs.mkdirSync(path.join(work, '.git'), { recursive: true })
    fs.mkdirSync(path.join(work, 'sub'), { recursive: true })
    fs.mkdirSync(agentsHome, { recursive: true })
    fs.writeFileSync(path.join(work, 'AGENTS.md'), '# Root rules\nROOT_RULE\n')
    fs.writeFileSync(path.join(work, 'sub', 'AGENTS.md'), '# Nested rules\nNESTED_RULE\n')
    fs.writeFileSync(path.join(agentsHome, 'AGENTS.md'), '# Shared rules\nSHARED_RULE\n')
    installProfile(dshHome, dataRoot)

    const hosts = []
    let model
    t.after(async () => {
      for (const host of hosts) {
        if (host.child.exitCode === null) host.child.kill('SIGKILL')
      }
      await model?.close()
      fs.rmSync(root, { recursive: true, force: true })
    })

    model = await startModel((requests, response) => {
      if (requests.agent === 1) {
        toolUseResponse(response, 'read', { file_path: 'sub/AGENTS.md' })
      } else {
        textResponse(response, 'done')
      }
    })

    const sessionId = `session-${randomUUID()}`
    const host = startHost({
      work,
      dshHome,
      agentsHome,
      dataRoot,
      sessionId,
      mode: 'create',
      generation: 1,
      nonce: 'nonce-1',
      modelPort: model.port,
    })
    hosts.push(host)

    await host.waitFor((frame) => frame.method === 'nano.hello')
    await host.request('nano.subscribe', {
      address: { rootSessionId: sessionId },
      kinds: ['session.event', 'instructions.changed'],
    })
    const submitted = await host.request('nano.conversation.submit', {
      address: { rootSessionId: sessionId },
      requestId: `nested-${randomUUID()}`,
      delivery: 'queue',
      content: [{ type: 'text', text: 'read the nested rules, then answer' }],
    })
    assert.equal(submitted.result.accepted, true)

    const nested = await host.waitFor(
      (frame) =>
        frame.method === 'nano.event' &&
        frame.params.kind === 'instructions.changed' &&
        frame.params.payload.sources.some((source) => source.path === 'sub/AGENTS.md'),
      60_000,
    )
    const paths = nested.params.payload.sources.map((source) => source.path)
    assert.ok(paths.includes('AGENTS.md'), `root instruction missing from ${paths.join(', ')}`)
    assert.ok(paths.includes('sub/AGENTS.md'), `nested instruction missing from ${paths.join(', ')}`)
    // The real post-tool agent request must carry the nested body; the
    // session-title auxiliary request is classified separately and ignored.
    await waitForCondition(
      () => model.requests.bodies.some((body) => body.includes('NESTED_RULE')),
      30_000,
      'the post-tool model request containing NESTED_RULE',
    )
    assert.ok(model.requests.agent >= 2, 'the nested file must reach a second agent request')
    const toolEvent = await host.waitFor(
      (frame) => sessionEvent(frame, 'tool/result') || sessionEvent(frame, 'tool/call'),
    )
    assert.ok(toolEvent)

    await host.stop()
    const agentRequestsAfterFirst = model.requests.agent

    // Resume after a real file change: the next pre-step must reconcile the
    // baseline, and the changed body must reach the next model request.
    fs.writeFileSync(path.join(work, 'AGENTS.md'), '# Root rules\nROOT_RULE\nCHANGED_RULE\n')
    const resumed = startHost({
      work,
      dshHome,
      agentsHome,
      dataRoot,
      sessionId,
      mode: 'resume',
      generation: 2,
      nonce: 'nonce-2',
      modelPort: model.port,
    })
    hosts.push(resumed)
    await resumed.waitFor((frame) => frame.method === 'nano.hello')
    await resumed.request('nano.subscribe', {
      address: { rootSessionId: sessionId },
      kinds: ['session.event', 'instructions.changed'],
    })
    const before = await resumed.request('nano.instructions.snapshot', {
      address: { rootSessionId: sessionId },
    })
    const beforePaths = before.result.sources.map((source) => source.path)
    assert.ok(beforePaths.includes('AGENTS.md'), `resume lost the root source: ${beforePaths.join(', ')}`)
    assert.ok(
      beforePaths.includes('sub/AGENTS.md'),
      `resume lost the nested source: ${beforePaths.join(', ')}`,
    )
    assert.equal(model.requests.agent, agentRequestsAfterFirst, 'the snapshot itself makes no model request')

    const resumeStart = resumed.frames.length
    await resumed.request('nano.conversation.submit', {
      address: { rootSessionId: sessionId },
      requestId: `resume-${randomUUID()}`,
      delivery: 'queue',
      content: [{ type: 'text', text: 'answer after resume' }],
    })
    const corrected = await resumed.waitFor(
      (frame, index) =>
        index >= resumeStart &&
        frame.method === 'nano.event' &&
        frame.params.kind === 'instructions.changed',
      60_000,
    )
    assert.ok(corrected)
    await waitForCondition(
      () => model.requests.bodies.some((body) => body.includes('CHANGED_RULE')),
      30_000,
      'the resumed model request containing CHANGED_RULE',
    )
    assert.ok(model.requests.agent > agentRequestsAfterFirst, 'resume must produce a real model request')
    const after = await resumed.request('nano.instructions.snapshot', {
      address: { rootSessionId: sessionId },
    })
    assert.deepEqual(
      after.result.sources.find((source) => source.path === 'AGENTS.md')?.action,
      'replace',
    )
    await resumed.stop()
  },
)

test(
  'a root-marker failure surfaces the official error and builds no sources',
  { skip: process.env.NANO_E2E !== '1' ? 'set NANO_E2E=1' : false, timeout: 180_000 },
  async (t) => {
    const root = fs.mkdtempSync(path.join(os.tmpdir(), 'nano-s05-marker-'))
    const dshHome = path.join(root, 'dsh-home')
    const agentsHome = path.join(root, 'agents-home')
    const dataRoot = path.join(root, 'data')
    const work = path.join(root, 'work')
    fs.mkdirSync(work, { recursive: true })
    // A self-referential marker makes the root-marker probe fail with ELOOP
    // instead of answering "absent"; the official loader must surface it.
    fs.symlinkSync('.git', path.join(work, '.git'))
    fs.mkdirSync(agentsHome, { recursive: true })
    fs.writeFileSync(path.join(work, 'AGENTS.md'), '# Root rules\nSECRET_RULE\n')
    installProfile(dshHome, dataRoot)

    const hosts = []
    let model
    t.after(async () => {
      for (const host of hosts) {
        if (host.child.exitCode === null) host.child.kill('SIGKILL')
      }
      await model?.close()
      fs.rmSync(root, { recursive: true, force: true })
    })
    model = await startModel((_requests, response) => textResponse(response, 'ok'))
    const sessionId = `session-${randomUUID()}`
    const host = startHost({
      work,
      dshHome,
      agentsHome,
      dataRoot,
      sessionId,
      mode: 'create',
      generation: 1,
      nonce: 'nonce-1',
      modelPort: model.port,
    })
    hosts.push(host)
    await host.waitFor((frame) => frame.method === 'nano.hello')
    await host.request('nano.subscribe', {
      address: { rootSessionId: sessionId },
      kinds: ['session.event'],
    })
    await host.request('nano.conversation.submit', {
      address: { rootSessionId: sessionId },
      requestId: `marker-${randomUUID()}`,
      delivery: 'queue',
      content: [{ type: 'text', text: 'say ok' }],
    })
    const end = await host.waitFor((frame) => sessionEvent(frame, 'turn/end'))
    assert.equal(end.params.payload.data.reason.kind, 'error')
    const message = end.params.payload.data.reason.error.message
    assert.match(message, /ELOOP|too many levels/i)
    assert.equal(model.requests.agent, 0, 'the official pre-step failed before any model request')
    const snapshot = await host.request('nano.instructions.snapshot', {
      address: { rootSessionId: sessionId },
    })
    assert.equal(snapshot.result.available, false)
    assert.deepEqual(snapshot.result.sources, [])
    await host.stop()
  },
)

test(
  'an unreadable base file is skipped while a readable local overlay loads',
  {
    skip:
      process.env.NANO_E2E !== '1'
        ? 'set NANO_E2E=1'
        : isRoot
          ? 'chmod 000 is ineffective for root'
          : false,
    timeout: 180_000,
  },
  async (t) => {
    const root = fs.mkdtempSync(path.join(os.tmpdir(), 'nano-s05-file-perm-'))
    const dshHome = path.join(root, 'dsh-home')
    const agentsHome = path.join(root, 'agents-home')
    const dataRoot = path.join(root, 'data')
    const work = path.join(root, 'work')
    fs.mkdirSync(path.join(work, '.git'), { recursive: true })
    fs.mkdirSync(agentsHome, { recursive: true })
    const base = path.join(work, 'AGENTS.md')
    fs.writeFileSync(base, '# Root rules\nSECRET_RULE\n')
    fs.chmodSync(base, 0o000)
    // Positive controls: a readable overlay proves discovery reached the
    // directory; an ancestor file outside the project root must never be used.
    fs.writeFileSync(path.join(work, 'AGENTS.local.md'), '# Local rules\nLOCAL_RULE\n')
    fs.writeFileSync(path.join(root, 'AGENTS.md'), '# Ancestor rules\nANCESTOR_RULE\n')
    installProfile(dshHome, dataRoot)

    const hosts = []
    let model
    t.after(async () => {
      for (const host of hosts) {
        if (host.child.exitCode === null) host.child.kill('SIGKILL')
      }
      await model?.close()
      fs.rmSync(root, { recursive: true, force: true })
    })
    model = await startModel((_requests, response) => textResponse(response, 'ok'))
    const sessionId = `session-${randomUUID()}`
    const host = startHost({
      work,
      dshHome,
      agentsHome,
      dataRoot,
      sessionId,
      mode: 'create',
      generation: 1,
      nonce: 'nonce-1',
      modelPort: model.port,
    })
    hosts.push(host)
    await host.waitFor((frame) => frame.method === 'nano.hello')
    await host.request('nano.subscribe', {
      address: { rootSessionId: sessionId },
      kinds: ['session.event'],
    })
    await host.request('nano.conversation.submit', {
      address: { rootSessionId: sessionId },
      requestId: `file-perm-${randomUUID()}`,
      delivery: 'queue',
      content: [{ type: 'text', text: 'say ok' }],
    })
    const end = await host.waitFor((frame) => sessionEvent(frame, 'turn/end'))
    assert.equal(end.params.payload.data.reason.kind, 'completed')
    await waitForCondition(
      () => model.requests.bodies.some((body) => body.includes('LOCAL_RULE')),
      30_000,
      'the model request containing the readable local overlay',
    )
    assert.ok(!model.requests.bodies.some((body) => body.includes('ANCESTOR_RULE')))
    assert.ok(!model.requests.bodies.some((body) => body.includes('SECRET_RULE')))
    const snapshot = await host.request('nano.instructions.snapshot', {
      address: { rootSessionId: sessionId },
    })
    assert.equal(snapshot.result.available, true)
    assert.deepEqual(
      snapshot.result.sources.map((source) => source.path),
      ['AGENTS.local.md'],
    )
    await host.stop()
  },
)
