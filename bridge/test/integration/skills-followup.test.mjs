// S05 follow-up real-host evidence: nested instructions discovered through a
// real first-party read tool call, the same sources after a real --resume, and
// a real permission failure that must not fabricate an empty instruction set.
//
// Runs only with NANO_E2E=1. The DeepSeek route points at a local Messages
// mock, so the real pre-step and tools run without any model spend.
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
  return dsh
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
  const server = http.createServer((request, response) => {
    let body = ''
    request.on('data', (chunk) => {
      body += chunk
    })
    request.on('end', () => handler(body, response))
  })
  return new Promise((resolve) => {
    server.listen(0, '127.0.0.1', () => {
      resolve({
        port: server.address().port,
        close: () => new Promise((done) => server.close(done)),
      })
    })
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
        reject(new Error(`timed out waiting for a frame; stderr: ${stderr.slice(-2000)}`))
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
    const exit = await new Promise((resolve) => child.once('exit', (code) => resolve(code)))
    assert.equal(exit, 0)
  }
  return { child, frames, waitFor, request, stop }
}

function sessionEvent(frame, type) {
  return (
    frame.method === 'nano.event' &&
    frame.params.kind === 'session.event' &&
    frame.params.payload.type === type
  )
}

test(
  'nested instructions and the resumed instruction baseline',
  { skip: process.env.NANO_E2E !== '1' ? 'set NANO_E2E=1' : false, timeout: 240_000 },
  async (t) => {
    const root = fs.mkdtempSync(path.join(os.tmpdir(), 'nano-s05-followup-'))
    t.after(() => fs.rmSync(root, { recursive: true, force: true }))
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

    let requests = 0
    const model = await startModel((body, response) => {
      requests += 1
      if (requests === 1) {
        toolUseResponse(response, 'read', { file_path: 'sub/AGENTS.md' })
      } else {
        textResponse(response, 'done')
      }
    })
    t.after(() => model.close())

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
    t.after(() => {
      if (host.child.exitCode === null) host.child.kill('SIGKILL')
    })

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
    // The first read is the root baseline; the second request is the model's
    // post-tool answer, proving the nested file was injected between them.
    assert.equal(requests, 2)
    const toolResult = await host.waitFor(
      (frame) => sessionEvent(frame, 'tool/result') || sessionEvent(frame, 'tool/call'),
    )
    assert.ok(toolResult)

    await host.stop()
    const requestsAfterFirst = requests

    // A real resume reads the durable log; the instruction projection must
    // already know the sources without a new model request.
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
    t.after(() => {
      if (resumed.child.exitCode === null) resumed.child.kill('SIGKILL')
    })
    await resumed.waitFor((frame) => frame.method === 'nano.hello')
    const snapshot = await resumed.request('nano.instructions.snapshot', {
      address: { rootSessionId: sessionId },
    })
    const resumedPaths = snapshot.result.sources.map((source) => source.path)
    assert.ok(resumedPaths.includes('AGENTS.md'), `resume lost the root source: ${resumedPaths.join(', ')}`)
    assert.ok(
      resumedPaths.includes('sub/AGENTS.md'),
      `resume lost the nested source: ${resumedPaths.join(', ')}`,
    )
    assert.equal(requests, requestsAfterFirst, 'resume must not call the model')
    await resumed.stop()
  },
)

test(
  'a root-marker failure surfaces the official error and builds no sources',
  { skip: process.env.NANO_E2E !== '1' ? 'set NANO_E2E=1' : false, timeout: 180_000 },
  async (t) => {
    const root = fs.mkdtempSync(path.join(os.tmpdir(), 'nano-s05-perm-'))
    t.after(() => fs.rmSync(root, { recursive: true, force: true }))
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

    let requests = 0
    const model = await startModel((_body, response) => {
      requests += 1
      textResponse(response, 'ok')
    })
    t.after(() => model.close())
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
    t.after(() => {
      if (host.child.exitCode === null) host.child.kill('SIGKILL')
    })
    await host.waitFor((frame) => frame.method === 'nano.hello')
    await host.request('nano.subscribe', {
      address: { rootSessionId: sessionId },
      kinds: ['session.event'],
    })
    await host.request('nano.conversation.submit', {
      address: { rootSessionId: sessionId },
      requestId: `perm-${randomUUID()}`,
      delivery: 'queue',
      content: [{ type: 'text', text: 'say ok' }],
    })
    const end = await host.waitFor((frame) => sessionEvent(frame, 'turn/end'))
    assert.equal(end.params.payload.data.reason.kind, 'error')
    const message = end.params.payload.data.reason.error.message
    assert.match(message, /ELOOP|symlink|too many levels|\.git/i)
    assert.equal(requests, 0, 'the official pre-step failed before any model request')
    const snapshot = await host.request('nano.instructions.snapshot', {
      address: { rootSessionId: sessionId },
    })
    assert.equal(snapshot.result.available, false)
    assert.deepEqual(snapshot.result.sources, [])
    await host.stop()
  },
)


test(
  'an unreadable rule file is skipped without fabricating sources',
  { skip: process.env.NANO_E2E !== '1' ? 'set NANO_E2E=1' : false, timeout: 180_000 },
  async (t) => {
    const root = fs.mkdtempSync(path.join(os.tmpdir(), 'nano-s05-file-perm-'))
    t.after(() => fs.rmSync(root, { recursive: true, force: true }))
    const dshHome = path.join(root, 'dsh-home')
    const agentsHome = path.join(root, 'agents-home')
    const dataRoot = path.join(root, 'data')
    const work = path.join(root, 'work')
    fs.mkdirSync(path.join(work, '.git'), { recursive: true })
    fs.mkdirSync(agentsHome, { recursive: true })
    const rules = path.join(work, 'AGENTS.md')
    fs.writeFileSync(rules, '# Root rules\nSECRET_RULE\n')
    fs.chmodSync(rules, 0o000)
    installProfile(dshHome, dataRoot)

    let requests = 0
    const model = await startModel((_body, response) => {
      requests += 1
      textResponse(response, 'ok')
    })
    t.after(() => model.close())
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
    t.after(() => {
      if (host.child.exitCode === null) host.child.kill('SIGKILL')
    })
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
    assert.ok(requests >= 1)
    const snapshot = await host.request('nano.instructions.snapshot', {
      address: { rootSessionId: sessionId },
    })
    assert.equal(snapshot.result.available, false)
    assert.deepEqual(snapshot.result.sources, [])
    await host.stop()
  },
)
