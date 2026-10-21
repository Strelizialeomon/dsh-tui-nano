// Real-host S05 integration: a real profile discovers real skill files,
// previews the official path, reacts to the provider watcher, folds real
// agent-instructions messages, and receives the official `/hello` injection.
//
// Runs only with NANO_E2E=1 because it installs a profile and starts dsh. The
// temporary DSH_HOME has no credentials, so this test never calls a model; the
// pre-step still decides and logs the injection before the request fails.
import test from 'node:test'
import assert from 'node:assert/strict'
import fs from 'node:fs'
import os from 'node:os'
import path from 'node:path'
import { spawn, execFileSync } from 'node:child_process'
import http from 'node:http'
import { randomUUID } from 'node:crypto'
import { fileURLToPath } from 'node:url'

const bridgeRoot = fileURLToPath(new URL('../..', import.meta.url))
const installScript = path.join(bridgeRoot, 'profile', 'install.mjs')

test(
  'real host discovers, previews, invalidates, and injects skills',
  { skip: process.env.NANO_E2E !== '1' ? 'set NANO_E2E=1' : false, timeout: 180_000 },
  async (t) => {
    const root = fs.mkdtempSync(path.join(os.tmpdir(), 'nano-s05-e2e-'))
    const dshHome = path.join(root, 'dsh-home')
    const agentsHome = path.join(root, 'agents-home')
    const dataRoot = path.join(root, 'data')
    const work = path.join(root, 'work')
    fs.mkdirSync(path.join(work, '.git'), { recursive: true })
    fs.mkdirSync(path.join(work, '.agents', 'skills', 'hello'), { recursive: true })
    fs.mkdirSync(agentsHome, { recursive: true })
    const helloFile = fs.realpathSync(path.dirname(path.join(work, '.agents', 'skills', 'hello', 'SKILL.md')))
    const helloPath = path.join(helloFile, 'SKILL.md')
    fs.writeFileSync(
      helloPath,
      '---\nname: hello\ndescription: Says hello for the S05 fixture\n---\n# hello\nHELLO_MARKER\n',
    )
    const userOnlyDir = path.join(work, '.agents', 'skills', 'user-only')
    fs.mkdirSync(userOnlyDir, { recursive: true })
    fs.writeFileSync(
      path.join(userOnlyDir, 'SKILL.md'),
      '---\nname: user-only\ndescription: Only the user can invoke this fixture skill\ndisable-model-invocation: true\n---\n# user-only\nUSER_ONLY_MARKER\n',
    )
    fs.writeFileSync(path.join(agentsHome, 'AGENTS.md'), '# Shared rules\nS05_SHARED_RULE\n')
    const projectRules = '# Project rules\nS05_PROJECT_RULE\n'
    fs.writeFileSync(path.join(work, 'AGENTS.md'), projectRules)
    fs.writeFileSync(path.join(work, 'CLAUDE.md'), projectRules)
    fs.writeFileSync(path.join(work, 'AGENTS.local.md'), '# Local rules\nS05_LOCAL_RULE\n')

    const dsh = execFileSync('which', ['dsh'], { encoding: 'utf8' }).trim()
    const install = execFileSync(
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
    assert.equal(JSON.parse(install.trim()).installed, true)

    // A local Messages-compatible endpoint lets the real pre-step run (and
    // inject skills/instructions) without any real model spend or credential.
    const model = http.createServer((request, response) => {
      let body = ''
      request.on('data', (chunk) => {
        body += chunk
      })
      request.on('end', () => {
        response.writeHead(200, { 'content-type': 'text/event-stream' })
        const events = [
          { type: 'message_start', message: { usage: { input_tokens: 1, output_tokens: 0 } } },
          { type: 'content_block_start', index: 0, content_block: { type: 'text', text: '' } },
          { type: 'content_block_delta', index: 0, delta: { type: 'text_delta', text: 'ok' } },
          { type: 'content_block_stop', index: 0 },
          { type: 'message_delta', delta: { stop_reason: 'end_turn' }, usage: { output_tokens: 1 } },
          { type: 'message_stop' },
        ]
        for (const event of events) response.write(`event: ${event.type}\ndata: ${JSON.stringify(event)}\n\n`)
        response.end()
      })
    })
    await new Promise((resolve) => model.listen(0, '127.0.0.1', resolve))
    const modelPort = model.address().port
    t.after(() => model.close())

    const sessionId = `session-${randomUUID()}`
    const runtimeId = `rt-node-${randomUUID()}`
    const nonce = 'node-e2e-nonce'
    const patchPath = path.join(dataRoot, 'runtime', 's05-e2e.json')
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
            provider: 'deepseek-official',
            model: 'deepseek-flash',
          },
        },
      ]),
      { mode: 0o600 },
    )

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
    t.after(() => {
      if (child.exitCode === null) child.kill('SIGKILL')
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
      const id = `req-${++counter}`
      input.write(`${JSON.stringify({ jsonrpc: '2.0', id, method, params })}\n`)
      return waitFor(match ?? ((frame) => frame.id === id))
    }

    const hello = await waitFor((frame) => frame.method === 'nano.hello')
    assert.equal(hello.params.capabilities.skills.status, 'supported')
    const subscribe = await request('nano.subscribe', {
      address: { rootSessionId: sessionId },
      kinds: ['session.event', 'skills.changed', 'instructions.changed'],
    })
    assert.ok(subscribe.result.subscriptionId)

    const listed = await request('nano.skills.list', { address: { rootSessionId: sessionId } })
    assert.equal(listed.result.complete, true)
    const helloEntry = listed.result.skills.find((skill) => skill.name === 'hello')
    const userOnly = listed.result.skills.find((skill) => skill.name === 'user-only')
    assert.ok(helloEntry, 'hello skill is discovered')
    assert.equal(helloEntry.path, helloPath)
    assert.equal(helloEntry.modelInvocable, true)
    assert.ok(userOnly, 'user-only skill is discovered')
    assert.equal(userOnly.modelInvocable, false)
    assert.ok(listed.result.commands.includes('compact'), 'official commands are reported')

    const sessionEventsBefore = frames.filter(
      (frame) => frame.method === 'nano.event' && frame.params.kind === 'session.event',
    ).length
    const preview = await request('nano.skills.preview', {
      address: { rootSessionId: sessionId },
      name: 'hello',
    })
    assert.equal(preview.result.path, helloPath)
    assert.match(preview.result.text, /HELLO_MARKER/)
    assert.equal(preview.result.truncated, false)
    const missing = await request('nano.skills.preview', {
      address: { rootSessionId: sessionId },
      name: 'no-such-skill',
    })
    assert.equal(missing.error.code, 1019)
    const sessionEventsAfterPreview = frames.filter(
      (frame) => frame.method === 'nano.event' && frame.params.kind === 'session.event',
    ).length
    assert.equal(sessionEventsAfterPreview, sessionEventsBefore, 'preview writes no session event')

    fs.writeFileSync(
      helloPath,
      '---\nname: hello\ndescription: Watcher updated this description\n---\n# hello\nHELLO_MARKER\n',
    )
    const changed = await waitFor(
      (frame) =>
        frame.method === 'nano.event' &&
        frame.params.kind === 'skills.changed' &&
        frame.params.payload.reason === 'skills',
      20_000,
    )
    assert.ok(changed)
    const relisted = await request('nano.skills.list', { address: { rootSessionId: sessionId } })
    assert.equal(
      relisted.result.skills.find((skill) => skill.name === 'hello').description,
      'Watcher updated this description',
    )
    const beforeDelete = frames.length
    fs.rmSync(userOnlyDir, { recursive: true, force: true })
    await waitFor(
      (frame, index) =>
        index >= beforeDelete &&
        frame.method === 'nano.event' &&
        frame.params.kind === 'skills.changed' &&
        frame.params.payload.reason === 'skills',
      20_000,
    )
    const afterDelete = await request('nano.skills.list', { address: { rootSessionId: sessionId } })
    assert.equal(
      afterDelete.result.skills.some((skill) => skill.name === 'user-only'),
      false,
      'a deleted skill disappears from the next catalog',
    )

    const requestId = `skill-${randomUUID()}`
    const submitted = await request('nano.conversation.submit', {
      address: { rootSessionId: sessionId },
      requestId,
      delivery: 'queue',
      content: [{ type: 'text', text: '/hello' }],
    })
    assert.equal(submitted.result.accepted, true)

    const userMessage = await waitFor(
      (frame) =>
        frame.method === 'nano.event' &&
        frame.params.kind === 'session.event' &&
        frame.params.payload.type === 'user/message' &&
        frame.params.payload.data?.source?.rpcId === requestId,
    )
    assert.equal(userMessage.params.payload.data.content[0].text, '/hello')
    const injection = await waitFor(
      (frame) =>
        frame.method === 'nano.event' &&
        frame.params.kind === 'session.event' &&
        frame.params.payload.type === 'user/message' &&
        frame.params.payload.data?.source?.kind === 'skill-invocation',
      60_000,
    )
    assert.equal(injection.params.payload.data.source.name, 'hello')
    assert.match(JSON.stringify(injection.params.payload.data.content), /HELLO_MARKER/)

    const instructions = await waitFor(
      (frame) =>
        frame.method === 'nano.event' &&
        frame.params.kind === 'instructions.changed' &&
        frame.params.payload.sources.length > 0,
      60_000,
    )
    const sources = instructions.params.payload.sources.map((source) => source.path)
    assert.deepEqual(sources, ['$DSH_AGENTS_HOME/AGENTS.md', 'AGENTS.md', 'AGENTS.local.md'])
    assert.ok(!sources.includes('CLAUDE.md'), 'identical CLAUDE.md is collapsed by the loader')
    assert.equal('budgetNotice' in instructions.params.payload, false, 'small fixtures stay within budget')
    const snapshot = await request('nano.instructions.snapshot', {
      address: { rootSessionId: sessionId },
    })
    assert.equal(snapshot.result.available, true)
    assert.equal(snapshot.result.baseline, true)
    assert.ok(Number.isInteger(snapshot.result.loadedAt))

    const shutdown = await request('nano.shutdown', {})
    assert.equal(shutdown.result.clean, true)
    const exit = await new Promise((resolve) => child.once('exit', (code) => resolve(code)))
    assert.equal(exit, 0)
    fs.rmSync(root, { recursive: true, force: true })
  },
)
