#!/usr/bin/env node
// One authorized real-model session for S12 subagent catalog, navigation
// data, child transcript replay, and human continuation.
//
// This is evidence tooling, not a product or CI test. It installs a throwaway
// profile under the real DSH_HOME (using the user's own credentials), starts a
// real dsh host with the Nano bridge, asks the model to spawn one subagent,
// reads the official catalog through the Nano protocol, subscribes to the
// child address, and sends one human follow-up through subagents/prompt.
//
// Usage:
//   NANO_EVIDENCE_MODEL=deepseek-flash node scripts/model-evidence-s12.mjs
//
// Requires a built profile install entry and a signed-in ~/.dsh. The extra
// profile directory is removed on exit; existing profiles are not touched.
import fs from 'node:fs'
import os from 'node:os'
import path from 'node:path'
import { execFileSync, spawn } from 'node:child_process'
import { randomUUID } from 'node:crypto'
import { fileURLToPath } from 'node:url'

const ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..')
const INSTALL = path.join(ROOT, 'bridge', 'profile', 'install.mjs')
const DSH_HOME = process.env.DSH_HOME ?? path.join(os.homedir(), '.dsh')
const PROFILE = `nano-s12-evidence-${process.pid}`
const MODEL = process.env.NANO_EVIDENCE_MODEL ?? 'deepseek-flash'
const PROVIDER = process.env.NANO_EVIDENCE_PROVIDER ?? 'deepseek-official'
const TMP = fs.mkdtempSync(path.join(os.tmpdir(), 'nano-s12-model-'))
const WORK = path.join(TMP, 'work')
const DATA = path.join(TMP, 'data')
const PATCH = path.join(TMP, 'patch.json')
const PROFILE_DIR = path.join(DSH_HOME, 'profiles', PROFILE)
const RESULTS = []
fs.mkdirSync(WORK, { recursive: true })

function log(kind, value) {
  RESULTS.push({ kind, value })
  process.stdout.write(`${kind} ${JSON.stringify(value)}\n`)
}

function cleanup() {
  try {
    fs.rmSync(PROFILE_DIR, { recursive: true, force: true })
  } catch {
    // best-effort cleanup
  }
  fs.rmSync(TMP, { recursive: true, force: true })
}

function messageText(message) {
  if (!Array.isArray(message?.content)) return ''
  return message.content
    .map((block) => (typeof block?.text === 'string' ? block.text : ''))
    .join('')
}

async function main() {
  const dsh = execFileSync('which', ['dsh'], { encoding: 'utf8' }).trim()
  execFileSync(
    process.execPath,
    [
      INSTALL,
      '--dsh-home',
      DSH_HOME,
      '--data-root',
      DATA,
      '--bridge-package',
      path.join(ROOT, 'bridge'),
      '--profile',
      PROFILE,
      '--dsh-bin',
      dsh,
      '--json',
    ],
    { encoding: 'utf8' },
  )
  log('PROFILE', { profile: PROFILE, model: `${PROVIDER}/${MODEL}` })

  const sessionId = `session-${randomUUID()}`
  fs.writeFileSync(
    PATCH,
    JSON.stringify([
      { id: 'session-persistence-jsonl', config: { root: path.join(DATA, 'sessions') } },
      { id: 'storage-json', config: { root: path.join(DATA, 'runtime-data', sessionId, 'storage') } },
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
          runtimeId: `rt-s12-${randomUUID()}`,
          generation: 1,
          rootSessionId: sessionId,
          cwd: WORK,
          dataRoot: DATA,
          sessionMode: 'create',
          officialVersion: '0.2.1-rc.2',
          nonce: `s12-${randomUUID()}`,
          provider: PROVIDER,
          model: MODEL,
        },
      },
    ]),
    { mode: 0o600 },
  )

  const child = spawn(process.execPath, [dsh, '--profile', PROFILE, '--patch', PATCH], {
    cwd: WORK,
    env: { ...process.env, DSH_HOME },
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
      let frame
      try {
        frame = JSON.parse(line)
      } catch {
        continue
      }
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
  const exitPromise = new Promise((resolve) => child.once('exit', (code) => resolve(code)))

  const waitFor = (match, timeout = 120_000) =>
    new Promise((resolve, reject) => {
      const existing = frames.find(match)
      if (existing !== undefined) return resolve(existing)
      const waiter = { match, resolve }
      waiter.timer = setTimeout(() => {
        waiters.splice(waiters.indexOf(waiter), 1)
        const recent = frames.slice(-3).map((frame) => ({
          method: frame.method,
          id: frame.id,
          kind: frame.params?.kind,
          type: frame.params?.payload?.type,
          error: frame.error,
        }))
        reject(
          new Error(
            `timed out; recent=${JSON.stringify(recent)}; stderr=${stderr.slice(-800)}`,
          ),
        )
      }, timeout)
      waiters.push(waiter)
    })

  let counter = 0
  const call = async (method, params) => {
    const id = `req-${++counter}`
    input.write(`${JSON.stringify({ jsonrpc: '2.0', id, method, params })}\n`)
    return waitFor((frame) => frame.id === id)
  }
  const event = (kind, match) => (frame) =>
    frame.method === 'nano.event' &&
    frame.params.kind === kind &&
    (match === undefined || match(frame.params.payload))

  try {
    const hello = await waitFor(
      (frame) => frame.method === 'nano.hello' || frame.method === 'nano.startupError',
    )
    if (hello.method !== 'nano.hello') throw new Error(`startup failed: ${JSON.stringify(hello)}`)
    const address = { rootSessionId: sessionId }
    await call('nano.subscribe', {
      address,
      kinds: [
        'session.event',
        'conversation.status',
        'conversation.inbox',
        'conversation.stream',
        'subagents.event',
      ],
      fromSeq: 0,
    })

    const spawnPrompt =
      '这是 S12ROOTPROMPT。只做一件事：调用 subagent 工具（不要指定 provider），' +
      'label 设为 s12-child，任务内容为「只回复一行 S12CHILD，不要调用任何工具，不要读写文件」。' +
      '等它返回后，用一句很短的话结束。不要做其他事。'
    const spawnSubmit = await call('nano.conversation.submit', {
      address,
      requestId: `spawn-${randomUUID()}`,
      delivery: 'queue',
      content: [{ type: 'text', text: spawnPrompt }],
    })
    if (spawnSubmit.error) throw new Error(`spawn submit failed: ${JSON.stringify(spawnSubmit.error)}`)

    const spawnStart = await waitFor(
      event('subagents.event', (payload) => payload.type === 'start'),
      180_000,
    )
    const spawnId = spawnStart.params.payload.childId
    log('CHILD_STARTED', {
      childId: spawnId,
      provider: spawnStart.params.payload.provider,
      local: spawnStart.params.payload.local,
    })

    const spawnSnapshot = await call('nano.subagents.snapshot', {})
    log('SNAPSHOT', spawnSnapshot.result ?? spawnSnapshot.error)
    const spawnRow = spawnSnapshot.result?.agents?.find((row) => row.id === spawnId)
    if (spawnRow === undefined) throw new Error('spawn child is missing from the direct snapshot')
    if (spawnRow.mode !== 'continuable' && spawnRow.mode !== 'unknown') {
      throw new Error(`unexpected spawn child mode ${spawnRow.mode}`)
    }

    const spawnDescribed = await call('nano.subagents.describe', {
      parentSessionId: sessionId,
      childSessionId: spawnId,
    })
    log('DESCRIBED', spawnDescribed.result ?? spawnDescribed.error)
    if (spawnDescribed.result?.mode !== 'continuable') {
      throw new Error(`describe did not prove continuable mode: ${JSON.stringify(spawnDescribed)}`)
    }

    const childAddress = {
      rootSessionId: sessionId,
      parentSessionId: sessionId,
      childSessionId: spawnId,
      mode: 'continuable',
    }
    const spawnSubscription = await call('nano.subscribe', {
      address: childAddress,
      kinds: ['session.event'],
      fromSeq: 0,
    })
    log('CHILD_SUBSCRIBE', {
      subscriptionId: spawnSubscription.result?.subscriptionId,
      seq: spawnSubscription.result?.seq,
    })

    const spawnEnd = await waitFor(
      event('subagents.event', (payload) => payload.type === 'end' && payload.childId === spawnId),
      180_000,
    )
    log('CHILD_END', spawnEnd.params.payload)

    const childEvents = frames
      .filter(
        (frame) =>
          frame.method === 'nano.event' &&
          frame.params.kind === 'session.event' &&
          frame.params.payload.address?.childSessionId === spawnId,
      )
      .map((frame) => frame.params.payload)
    log('CHILD_EVENTS', {
      count: childEvents.length,
      types: [...new Set(childEvents.map((entry) => entry.type))].slice(0, 20),
      sawChildText: childEvents.some((entry) => JSON.stringify(entry.data).includes('S12CHILD')),
      sawParentSeed: childEvents.some((entry) => JSON.stringify(entry.data).includes('S12ROOTPROMPT')),
    })

    // Second root turn: a fork child that inherits the parent's completed
    // turns, including the distinctive S12ROOTPROMPT above.
    const forkPrompt =
      '只做一件事：调用 subagent_fork 工具，label 设为 s12-fork，' +
      '任务内容为「只回复一行 S12FORK，不要调用任何工具」。' +
      '等它返回后，用一句很短的话结束。不要做其他事。'
    const forkSubmit = await call('nano.conversation.submit', {
      address,
      requestId: `fork-${randomUUID()}`,
      delivery: 'queue',
      content: [{ type: 'text', text: forkPrompt }],
    })
    if (forkSubmit.error) throw new Error(`fork submit failed: ${JSON.stringify(forkSubmit.error)}`)

    const forkStart = await waitFor(
      event('subagents.event', (payload) => payload.type === 'start' && payload.childId !== spawnId),
      180_000,
    )
    const forkId = forkStart.params.payload.childId
    log('FORK_STARTED', {
      childId: forkId,
      provider: forkStart.params.payload.provider,
      local: forkStart.params.payload.local,
    })

    const forkAddress = {
      rootSessionId: sessionId,
      parentSessionId: sessionId,
      childSessionId: forkId,
      mode: 'continuable',
    }
    const forkDescribed = await call('nano.subagents.describe', {
      parentSessionId: sessionId,
      childSessionId: forkId,
    })
    log('FORK_DESCRIBED', forkDescribed.result ?? forkDescribed.error)
    if (forkDescribed.result?.mode !== 'continuable') {
      throw new Error(`fork describe did not prove continuable mode: ${JSON.stringify(forkDescribed)}`)
    }
    const forkSubscription = await call('nano.subscribe', {
      address: forkAddress,
      kinds: ['session.event'],
      fromSeq: 0,
    })
    log('FORK_SUBSCRIBE', {
      subscriptionId: forkSubscription.result?.subscriptionId,
      seq: forkSubscription.result?.seq,
    })
    const forkEnd = await waitFor(
      event('subagents.event', (payload) => payload.type === 'end' && payload.childId === forkId),
      180_000,
    )
    log('FORK_END', forkEnd.params.payload)

    const forkEvents = frames
      .filter(
        (frame) =>
          frame.method === 'nano.event' &&
          frame.params.kind === 'session.event' &&
          frame.params.payload.address?.childSessionId === forkId,
      )
      .map((frame) => frame.params.payload)
    log('FORK_EVENTS', {
      count: forkEvents.length,
      types: [...new Set(forkEvents.map((entry) => entry.type))].slice(0, 20),
      sawForkText: forkEvents.some((entry) => JSON.stringify(entry.data).includes('S12FORK')),
      sawParentSeed: forkEvents.some((entry) => JSON.stringify(entry.data).includes('S12ROOTPROMPT')),
    })

    const followup = await call('nano.subagents.prompt', {
      address: childAddress,
      requestId: `followup-${randomUUID()}`,
      delivery: 'queue',
      content: [{ type: 'text', text: '请只回复一行 S12FOLLOWUP，不要调用任何工具。' }],
    })
    log('FOLLOWUP', followup.result ?? followup.error)

    const secondEnd = await waitFor(
      event('subagents.event', (payload) => payload.type === 'end' && payload.childId === spawnId),
    )
    log('FOLLOWUP_END', secondEnd.params.payload)

    const interrupted = await call('nano.subagents.interrupt', { address: childAddress })
    log('INTERRUPT', interrupted.result ?? interrupted.error)

    const finalSnapshot = await call('nano.subagents.snapshot', {})
    log('FINAL_SNAPSHOT', finalSnapshot.result ?? finalSnapshot.error)

    const list = await call('nano.subagents.list', {})
    log('LIST', {
      entries: list.result?.entries?.map((entry) => ({
        id: entry.id,
        parentId: entry.parentId,
        depth: entry.depth,
        mode: entry.mode,
        activity: entry.activity,
      })),
      diagnostics: list.result?.diagnostics,
    })

    const shutdown = await call('nano.shutdown', {})
    log('SHUTDOWN', shutdown.result)
    const code = await Promise.race([
      exitPromise,
      new Promise((resolve) => setTimeout(() => resolve('timeout'), 20_000)),
    ])
    log('EXIT', code)
  } finally {
    if (child.exitCode === null) child.kill('SIGKILL')
  }
}

main()
  .catch((error) => {
    log('FAILED', { message: error instanceof Error ? error.message : String(error) })
    process.exitCode = 1
  })
  .finally(() => {
    for (const entry of RESULTS) {
      if (entry.kind === 'FAILED') process.stderr.write(`FAILED ${JSON.stringify(entry.value)}\n`)
    }
    cleanup()
  })
