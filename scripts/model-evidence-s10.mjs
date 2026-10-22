#!/usr/bin/env node
// One authorized real-model session for S10 Monitor delivery.
//
// This is evidence tooling, not a product or CI test. It installs a throwaway
// profile under the real DSH_HOME (so the user's own credentials are used, not
// copied), starts a real dsh host, asks the model to run two background jobs,
// then proves:
//   - quiet: line notifications enter the inbox without waking; a later user
//     message makes the durable user/message receipt observable.
//   - notify: followup wakes the agent and the durable user/message receipt is
//     observable.
//   - only matching lines are delivered; terminal state creates no S10 notice.
//   - the observed job is still present after the monitor stops.
//
// Usage:
//   NANO_EVIDENCE_MODEL=deepseek-flash node scripts/model-evidence-s10.mjs
//
// Requires a signed-in ~/.dsh and a built bridge. The extra profile directory
// is removed on exit; the user's existing profiles are not touched.
import fs from 'node:fs'
import os from 'node:os'
import path from 'node:path'
import { execFileSync, spawn } from 'node:child_process'
import { randomUUID } from 'node:crypto'
import { fileURLToPath } from 'node:url'

const ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..')
const INSTALL = path.join(ROOT, 'bridge', 'profile', 'install.mjs')
const DSH_HOME = process.env.DSH_HOME ?? path.join(os.homedir(), '.dsh')
const PROFILE = `nano-s10-evidence-${process.pid}`
const MODEL = process.env.NANO_EVIDENCE_MODEL ?? 'deepseek-flash'
const PROVIDER = process.env.NANO_EVIDENCE_PROVIDER ?? 'deepseek-official'
const TMP = fs.mkdtempSync(path.join(os.tmpdir(), 'nano-s10-model-'))
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
  log('PROFILE', { profile: PROFILE, provider: PROVIDER, model: MODEL })

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
      // Keep the official completion notice on the injection lane so the
      // evidence run is not charged for an extra completion wake. S10 must
      // still not suppress or duplicate it.
      { id: 'tool-jobs', config: { completionDelivery: 'quiet' } },
      {
        id: 'nano-bridge',
        config: {
          runtimeId: `rt-s10-${randomUUID()}`,
          generation: 1,
          rootSessionId: sessionId,
          cwd: WORK,
          dataRoot: DATA,
          sessionMode: 'create',
          officialVersion: '0.2.1-rc.2',
          nonce: `s10-${randomUUID()}`,
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
  const monitorEvents = []
  const userMessages = []
  const assistantMessages = []
  const collect = (frame) => {
    if (frame.method !== 'nano.event') return
    const { kind, payload } = frame.params
    if (kind === 'monitor.updated') monitorEvents.push(payload)
    if (kind === 'session.event' && payload.type === 'user/message') userMessages.push(payload.data)
    if (kind === 'session.event' && payload.type === 'assistant/message') {
      assistantMessages.push(payload.data)
    }
  }
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
      collect(frame)
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

  const waitFor = (match, timeout = 120_000) =>
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
    log('CAPABILITY', hello.params.capabilities.monitor)
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
        'monitor.updated',
      ],
      fromSeq: 0,
    })

    const beforeContext = await call('nano.sessions.context', { address })
    log('CONTEXT_BEFORE', beforeContext.result ?? beforeContext.error)

    const jobPrompt =
      "只做一件事：用 bash 工具运行命令 `sh -c 'sleep 3; echo monitor-line-1; echo noise-ignore-me; sleep 1; echo monitor-line-2; sleep 1; echo monitor-line-3; printf monitor-line-tail'`，" +
      '并把 run_in_background 设为 true。不要运行其他命令，不要读写文件。启动后用一句很短的话回复 job id，然后结束这一轮。'
    const first = await call('nano.conversation.submit', {
      address,
      requestId: `prompt-${randomUUID()}`,
      delivery: 'queue',
      content: [{ type: 'text', text: jobPrompt }],
    })
    if (first.error) throw new Error(`submit failed: ${JSON.stringify(first.error)}`)

    const registered = await waitFor(
      (frame) =>
        frame.method === 'nano.event' &&
        frame.params.kind === 'jobs.event' &&
        frame.params.payload.type === 'registered',
    )
    const job = registered.params.payload.job
    log('JOB_REGISTERED', { id: job.id, kind: job.kind, label: job.label })
    // Wait until line 1 is in the ring but the job is still running, so the
    // earliest attach has retained bytes to replay immediately.
    await new Promise((resolve) => setTimeout(resolve, 3500))
    const quietCreate = await call('nano.monitor.create', {
      address,
      jobId: job.id,
      from: 'earliest',
      mode: 'quiet',
      filter: { pattern: 'monitor-line', regex: false },
      budgets: { mergeWindowMs: 200, minIntervalMs: 200, maxNotifications: 20 },
    })
    if (quietCreate.error) throw new Error(`quiet create failed: ${JSON.stringify(quietCreate.error)}`)
    log('QUIET_CREATE', {
      unreadBytes: quietCreate.result.monitor.unreadBytes,
      skippedBytes: quietCreate.result.monitor.skippedBytes,
      offset: quietCreate.result.monitor.offset,
      mode: quietCreate.result.monitor.mode,
    })

    // Wait for the job to finish and the quiet delivery to be inserted. Quiet
    // does not wake, so the expected state is inserted until a user turn opens.
    const runningReplay = await waitFor(
      (frame) =>
        frame.method === 'nano.event' &&
        frame.params.kind === 'monitor.updated' &&
        frame.params.payload.monitor.jobId === job.id &&
        frame.params.payload.monitor.stream === 'watching' &&
        frame.params.payload.monitor.used.lines >= 1,
      30_000,
    )
    log('QUIET_RUNNING_REPLAY', {
      offset: runningReplay.params.payload.monitor.offset,
      stream: runningReplay.params.payload.monitor.stream,
      used: runningReplay.params.payload.monitor.used,
      texts: runningReplay.params.payload.monitor.deliveries.map((delivery) => delivery.textPreview),
    })

    // Wait for the whole job: four matching lines, one noise line. Quiet must
    // leave every delivery merely inserted until a user turn opens.
    const quietSettled = await waitFor(
      (frame) =>
        frame.method === 'nano.event' &&
        frame.params.kind === 'jobs.event' &&
        frame.params.payload.type === 'settled' &&
        frame.params.payload.job.id === job.id,
      90_000,
    )
    log('QUIET_JOB_SETTLED', {
      status: quietSettled.params.payload.job.status,
      detail: quietSettled.params.payload.job.detail,
    })
    const quietReady = await waitFor(
      (frame) =>
        frame.method === 'nano.event' &&
        frame.params.kind === 'monitor.updated' &&
        frame.params.payload.monitor.jobId === job.id &&
        frame.params.payload.monitor.used.lines >= 4 &&
        frame.params.payload.monitor.stream !== 'watching',
      60_000,
    )
    const quietMonitor = quietReady.params.payload.monitor
    log('QUIET_DELIVERY', {
      states: quietMonitor.deliveries.map((delivery) => delivery.state),
      texts: quietMonitor.deliveries.map((delivery) => delivery.textPreview),
      used: quietMonitor.used,
      stream: quietMonitor.stream,
      streamDetail: quietMonitor.streamDetail,
      noiseDelivered: quietMonitor.deliveries.some((delivery) =>
        delivery.textPreview.includes('noise-ignore-me'),
      ),
    })

    // One ordinary user turn makes the injected quiet context part of an
    // admitted step. The durable user/message id is the proof.
    const ask = await call('nano.conversation.submit', {
      address,
      requestId: `prompt-${randomUUID()}`,
      delivery: 'queue',
      content: [
        {
          type: 'text',
          text: '刚才的监听通知里出现了哪些 monitor-line 行？只列出数字，不要调用工具。',
        },
      ],
    })
    if (ask.error) throw new Error(`ask failed: ${JSON.stringify(ask.error)}`)
    const quietAccepted = await waitFor(
      (frame) =>
        frame.method === 'nano.event' &&
        frame.params.kind === 'monitor.updated' &&
        frame.params.payload.monitor.jobId === job.id &&
        frame.params.payload.monitor.used.lines >= 4 &&
        frame.params.payload.monitor.deliveries.length > 0 &&
        frame.params.payload.monitor.deliveries.every((delivery) => delivery.state === 'accepted'),
      120_000,
    )
    log('QUIET_RECEIPT', {
      deliveries: quietAccepted.params.payload.monitor.deliveries.map((delivery) => ({
        state: delivery.state,
        messageId: delivery.messageId,
        lines: delivery.lines,
        text: delivery.textPreview,
      })),
    })
    const sawMonitorMessage = userMessages.some(
      (message) => message?.source?.kind === 'monitor',
    )
    const monitorMessages = userMessages.filter(
      (message) => message?.source?.kind === 'monitor',
    )
    const monitorTexts = monitorMessages.map((message) => messageText(message))
    log('QUIET_DURABLE_USER_MESSAGE', {
      present: sawMonitorMessage,
      monitorMessageIds: monitorMessages.map((message) => message.id),
      tailPresent: monitorTexts.some((text) => text.includes('monitor-line-tail')),
      texts: monitorTexts,
    })
    const completionNotices = (jobId) => {
      const matches = userMessages.filter((message) =>
        messageText(message).includes(`background job ${jobId}`),
      )
      const unique = [...new Map(matches.map((message) => [message.id, message])).values()]
      return {
        count: unique.length,
        ids: unique.map((message) => message.id),
        texts: unique.map((message) => messageText(message).slice(0, 300)),
      }
    }
    log('OFFICIAL_COMPLETION_NOTICE', {
      jobId: job.id,
      ...completionNotices(job.id),
    })

    // Second job: notify mode. The followup itself wakes the agent, so the
    // durable user/message receipt arrives without another user prompt.
    const notifyPrompt =
      "只做一件事：用 bash 工具运行命令 `sh -c 'sleep 2; echo notify-line-42'`，" +
      '并把 run_in_background 设为 true。不要运行其他命令，不要读写文件。启动后用一句很短的话回复 job id，然后结束这一轮。'
    const second = await call('nano.conversation.submit', {
      address,
      requestId: `prompt-${randomUUID()}`,
      delivery: 'queue',
      content: [{ type: 'text', text: notifyPrompt }],
    })
    if (second.error) throw new Error(`second submit failed: ${JSON.stringify(second.error)}`)
    const secondRegistered = await waitFor(
      (frame) =>
        frame.method === 'nano.event' &&
        frame.params.kind === 'jobs.event' &&
        frame.params.payload.type === 'registered' &&
        frame.params.payload.job.id !== job.id,
    )
    const notifyJob = secondRegistered.params.payload.job
    log('NOTIFY_JOB', { id: notifyJob.id, label: notifyJob.label })
    const notifyCreate = await call('nano.monitor.create', {
      address,
      jobId: notifyJob.id,
      from: 'new',
      mode: 'notify',
      filter: { pattern: 'notify-line', regex: false },
      budgets: { mergeWindowMs: 100, minIntervalMs: 0, maxNotifications: 5 },
    })
    if (notifyCreate.error) throw new Error(`notify create failed: ${JSON.stringify(notifyCreate.error)}`)
    const notifyAccepted = await waitFor(
      (frame) =>
        frame.method === 'nano.event' &&
        frame.params.kind === 'monitor.updated' &&
        frame.params.payload.monitor.jobId === notifyJob.id &&
        frame.params.payload.monitor.deliveries.some((delivery) => delivery.state === 'accepted'),
      120_000,
    )
    log('NOTIFY_RECEIPT', {
      deliveries: notifyAccepted.params.payload.monitor.deliveries.map((delivery) => ({
        state: delivery.state,
        mode: delivery.mode,
        messageId: delivery.messageId,
        lines: delivery.lines,
        text: delivery.textPreview,
      })),
    })
    // Open one tiny turn so a quiet-injected official completion notice is
    // claimed and becomes a durable user/message.
    await call('nano.conversation.submit', {
      address,
      requestId: `prompt-${randomUUID()}`,
      delivery: 'queue',
      content: [{ type: 'text', text: '请只回复 ok。' }],
    })
    try {
      await waitFor(
        (frame) =>
          frame.method === 'nano.event' &&
          frame.params.kind === 'session.event' &&
          frame.params.payload.type === 'user/message' &&
          messageText(frame.params.payload.data).includes(`background job ${notifyJob.id}`),
        60_000,
      )
    } catch {
      // The completion notice may remain pending; the log below reports it.
    }
    log('OFFICIAL_COMPLETION_NOTICE', {
      jobId: notifyJob.id,
      ...completionNotices(notifyJob.id),
    })

    // Stop the quiet monitor and prove the job is untouched.
    const stop = await call('nano.monitor.stop', { address, jobId: job.id })
    log('STOP_MONITOR', stop.result?.monitor?.stoppedByUser ?? stop.error)
    const jobsAfterStop = await call('nano.jobs.snapshot', { address })
    log('JOB_STILL_PRESENT', {
      first: jobsAfterStop.result.jobs.some((entry) => entry.id === job.id),
      second: jobsAfterStop.result.jobs.some((entry) => entry.id === notifyJob.id),
    })

    const afterContext = await call('nano.sessions.context', { address })
    log('CONTEXT_AFTER', afterContext.result ?? afterContext.error)
    log(
      'ASSISTANT_TEXTS',
      assistantMessages
        .map((data) => messageText(data?.message ?? data).slice(0, 200))
        .filter(Boolean),
    )

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
