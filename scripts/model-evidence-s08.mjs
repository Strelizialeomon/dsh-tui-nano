#!/usr/bin/env node
// One authorized real-model session for S08 job observation and human stop.
//
// This is evidence tooling, not a product or CI test. It installs a throwaway
// profile under the real DSH_HOME (so the user's own credentials are used, not
// copied), starts a real dsh host, asks the model once to run a long command in
// the background, observes the official job, kills it with the human reason,
// and prints the official settlement plus the model-facing stop/notice fact.
//
// Usage:
//   NANO_EVIDENCE_MODEL=deepseek-flash node scripts/model-evidence-s08.mjs
//
// Requires a built profile install entry and a signed-in `~/.dsh`. The extra
// profile directory is removed on exit; the user's existing profiles are not
// touched.
import fs from 'node:fs'
import os from 'node:os'
import path from 'node:path'
import { execFileSync, spawn } from 'node:child_process'
import { randomUUID } from 'node:crypto'
import { fileURLToPath } from 'node:url'

const ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..')
const INSTALL = path.join(ROOT, 'bridge', 'profile', 'install.mjs')
const DSH_HOME = process.env.DSH_HOME ?? path.join(os.homedir(), '.dsh')
const PROFILE = `nano-s08-evidence-${process.pid}`
const MODEL = process.env.NANO_EVIDENCE_MODEL ?? 'deepseek-flash'
const PROVIDER = process.env.NANO_EVIDENCE_PROVIDER ?? 'deepseek-official'
const TMP = fs.mkdtempSync(path.join(os.tmpdir(), 'nano-s08-model-'))
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

/** Extract a completion notice from an inbox splice or inbox projection. */
function noticeText(payload) {
  const inserted = payload?.data?.inserted
  const texts = []
  if (Array.isArray(inserted)) {
    for (const message of inserted) texts.push(messageText(message))
  }
  const inbox = payload?.inbox
  if (inbox !== undefined) {
    for (const message of [...(inbox.nextTurn ?? []), ...(inbox.nextStep ?? [])]) {
      texts.push(messageText(message))
    }
  }
  const notice = texts.find((text) => text.includes('background job'))
  return notice === undefined ? undefined : notice
}

function messageText(message) {
  if (!Array.isArray(message?.content)) return ''
  return message.content
    .map((block) => (typeof block?.text === 'string' ? block.text : ''))
    .join('')
}

function cleanup() {
  try {
    fs.rmSync(PROFILE_DIR, { recursive: true, force: true })
  } catch {
    // best-effort cleanup
  }
  fs.rmSync(TMP, { recursive: true, force: true })
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
      // Deliver the completion notice on the injection lane instead of opening
      // a second model turn. The one-notice-channel fact is still official
      // `dsh-tool-jobs` behavior; this only keeps the evidence run cheap.
      { id: 'tool-jobs', config: { completionDelivery: 'quiet' } },
      {
        id: 'nano-bridge',
        config: {
          runtimeId: `rt-s08-${randomUUID()}`,
          generation: 1,
          rootSessionId: sessionId,
          cwd: WORK,
          dataRoot: DATA,
          sessionMode: 'create',
          officialVersion: '0.2.1-rc.2',
          nonce: `s08-${randomUUID()}`,
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
  const exitPromise = new Promise((resolve) => {
    child.once('exit', (code) => resolve(code))
  })

  const waitFor = (match, timeout = 90_000) =>
    new Promise((resolve, reject) => {
      const existing = frames.find(match)
      if (existing !== undefined) return resolve(existing)
      const waiter = { match, resolve }
      waiter.timer = setTimeout(() => {
        waiters.splice(waiters.indexOf(waiter), 1)
        reject(new Error(`timed out; recent=${JSON.stringify(frames.slice(-6))}; stderr=${stderr.slice(-1000)}`))
      }, timeout)
      waiters.push(waiter)
    })

  let counter = 0
  const call = async (method, params) => {
    const id = `req-${++counter}`
    input.write(`${JSON.stringify({ jsonrpc: '2.0', id, method, params })}\n`)
    return waitFor((frame) => frame.id === id)
  }

  try {
    const hello = await waitFor((frame) => frame.method === 'nano.hello' || frame.method === 'nano.startupError')
    if (hello.method !== 'nano.hello') throw new Error(`startup failed: ${JSON.stringify(hello)}`)
    const address = { rootSessionId: sessionId }
    await call('nano.subscribe', {
      address,
      kinds: [
        'session.event',
        'conversation.status',
        'conversation.inbox',
        'conversation.stream',
        'jobs.event',
        'jobs.output',
      ],
      fromSeq: 0,
    })

    const prompt =
      "只做一件事：用 bash 工具运行命令 `sh -c 'echo s08-start; sleep 40; echo s08-end'`，" +
      '并把 run_in_background 设为 true。不要运行其他命令，不要读写文件。' +
      '启动后用一句很短的话回复 job id，然后结束这一轮。'
    const requestId = `prompt-${randomUUID()}`
    const submit = await call('nano.conversation.submit', {
      address,
      requestId,
      delivery: 'queue',
      content: [{ type: 'text', text: prompt }],
    })
    if (submit.error) throw new Error(`submit failed: ${JSON.stringify(submit.error)}`)

    const registered = await waitFor(
      (frame) =>
        frame.method === 'nano.event' &&
        frame.params.kind === 'jobs.event' &&
        frame.params.payload.type === 'registered',
      120_000,
    )
    const job = registered.params.payload.job
    log('JOB_REGISTERED', { id: job.id, kind: job.kind, label: job.label, status: job.status })

    // Give the background pull source one pump round, then read without
    // consuming anything the model may later read.
    await new Promise((resolve) => setTimeout(resolve, 400))
    const snapshot = await call('nano.jobs.snapshot', { address })
    const listed = snapshot.result.jobs.find((entry) => entry.id === job.id)
    log('SNAPSHOT', listed ?? { id: job.id, missing: true })
    const follow = await call('nano.jobs.follow', { address, jobId: job.id, from: 0 })
    log('FOLLOW', {
      chunks: follow.result.chunks.map((chunk) => ({
        at: chunk.at,
        channel: chunk.channel,
        text: chunk.text.slice(0, 120),
      })),
      next: follow.result.next,
      lossy: follow.result.lossy,
    })

    const kill = await call('nano.jobs.kill', { address, jobId: job.id })
    log('KILL', kill.result ?? kill.error)

    const settled = await waitFor(
      (frame) =>
        frame.method === 'nano.event' &&
        frame.params.kind === 'jobs.event' &&
        frame.params.payload.type === 'settled' &&
        frame.params.payload.job.id === job.id,
      90_000,
    )
    log('SETTLED', {
      status: settled.params.payload.job.status,
      detail: settled.params.payload.job.detail,
      cause: settled.params.payload.cause,
      awaited: settled.params.payload.awaited,
    })

    // The one model-facing channel: an awaited foreground result carries
    // `[stopped: ...]`; an unawaited background settlement produces exactly
    // one completion notice. Lifecycle records (`job/settled`) are not the
    // model-facing channel and are excluded from this count.
    try {
      await waitFor(
        (frame) =>
          frame.method === 'nano.event' &&
          ['session.event', 'conversation.inbox'].includes(frame.params.kind) &&
          noticeText(frame.params.payload) !== undefined,
        30_000,
      )
      const notices = frames
        .filter(
          (frame) =>
            frame.method === 'nano.event' &&
            ['session.event', 'conversation.inbox'].includes(frame.params.kind),
        )
        .map((frame) => noticeText(frame.params.payload))
        .filter((text) => text !== undefined)
      const unique = [...new Set(notices)]
      log('MODEL_FACING_STOP', {
        unique_notice_count: unique.length,
        carries_human_reason: unique.every((text) => text.includes('cancelled by the user')),
        notice: unique[0]?.slice(0, 500),
      })
    } catch (error) {
      log('MODEL_FACING_STOP', { missing: true, detail: String(error.message).split(';')[0] })
    }

    const shutdown = await call('nano.shutdown', {})
    log('SHUTDOWN', shutdown.result)
    const code = await Promise.race([exitPromise, new Promise((resolve) => setTimeout(() => resolve('timeout'), 20_000))])
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
