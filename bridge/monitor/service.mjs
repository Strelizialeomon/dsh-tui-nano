// S10 Monitor host: bounded, non-consuming observation of one official job's
// output ring.
//
// The monitor owns no process, no second output store, and no scheduler. It
// subscribes to the official job registry with `readAt` at its own absolute
// byte offset, turns matching lines into line notifications, and delivers them
// through the official Agent `followup` (wakes) or `inject` (does not wake)
// surfaces. Terminal job states only update this projection; the official
// `dsh-tool-jobs` completion notice remains the one terminal model notice.
//
// Delivery receipts are official facts:
//   submitted -> inserted -> claimed -> accepted (durable user/message)
// `inserted` and `claimed` are never reported as "the model received it"; only
// a durable `user/message` with the same id is `accepted`.
import { randomUUID } from 'node:crypto'
import { RpcError } from '../runtime/errors.mjs'

/** Implementation-time defaults; the spec leaves these to implementation. */
export const MONITOR_DEFAULTS = Object.freeze({
  mergeWindowMs: 800,
  minIntervalMs: 1000,
  maxNotifications: 50,
  maxLinesPerNotification: 20,
  maxNotificationBytes: 8192,
  maxNotificationTokens: 2048,
  expiresInMs: 30 * 60 * 1000,
})

/** Hard bounds so a user-supplied budget cannot make the host unbounded. */
export const MONITOR_LIMITS = Object.freeze({
  maxMonitors: 32,
  maxFilterBytes: 512,
  maxLineBytes: 16384,
  minMergeWindowMs: 100,
  maxMergeWindowMs: 60_000,
  minNotifications: 1,
  maxNotifications: 10_000,
  minIntervalMs: 0,
  maxIntervalMs: 60_000,
  minLines: 1,
  maxLines: 200,
  minBytes: 512,
  maxBytes: 65_536,
  minTokens: 128,
  maxTokens: 16_384,
  minExpiresMs: 1_000,
  maxExpiresMs: 24 * 60 * 60 * 1000,
  deliveryTimeoutMs: 120_000,
  maxReadyWindows: 20,
  publishThrottleMs: 500,
  maxDeliveriesKept: 20,
  maxDeliveryEntries: 4096,
})

const TERMINAL_STREAM = new Set(['stream-ended', 'failed', 'stopped'])

/**
 * One Monitor service per host generation.
 */
export class MonitorService {
  /**
   * @param {{
   *   ctx: object,
   *   hub: import('../runtime/events.mjs').SubscriptionHub,
   *   agent: object,
   *   session: object,
   *   rootSessionId: string,
   *   timers?: {setTimeout: Function, clearTimeout: Function, now: () => number},
   * }} options - host wiring; timers are injectable for deterministic tests.
   */
  constructor({ ctx, hub, agent, session, rootSessionId, timers }) {
    this.ctx = ctx
    this.hub = hub
    this.agent = agent
    this.session = session
    this.rootSessionId = rootSessionId
    this.address = { rootSessionId }
    this.registry = ctx.get('jobs')
    this.timers = timers ?? {
      setTimeout: (fn, ms) => {
        const timer = setTimeout(fn, ms)
        if (typeof timer.unref === 'function') timer.unref()
        return timer
      },
      clearTimeout: (timer) => clearTimeout(timer),
      now: () => Date.now(),
    }
    this.monitors = new Map()
    this.deliveries = new Map()
    this.disposers = []
    this.closed = false
    this.eventSeq = 0
    this.eventsAvailable = false
    this.publishTimers = new Map()
    this.listen()
  }

  /** Subscribe to official job output, inbox receipts, and durable messages. */
  listen() {
    if (this.registry !== undefined) {
      try {
        this.disposers.push(
          this.registry.events.subscribe({ owner: this.rootSessionId }, (event) => {
            this.onJobEvent(event)
          }),
        )
        this.eventsAvailable = true
      } catch {
        // A registry without an event stream cannot drive a live monitor;
        // `assertAvailable` reports `nano/monitor/unavailable` instead of
        // returning a monitor that can never see new output.
        this.eventsAvailable = false
      }
    }
    const onAgent = (event, handler) => {
      const disposer = this.ctx.on(event, (payload) => {
        if (this.closed) return
        if (payload?.agent === undefined || payload.agent.id !== this.rootSessionId) return
        handler(payload)
      })
      if (typeof disposer === 'function') this.disposers.push(disposer)
    }
    onAgent('agent/inbox/inserted', ({ message }) => this.onInbox(message, 'inserted'))
    onAgent('agent/inbox/claimed', ({ message, turn }) => this.onInbox(message, 'claimed', turn))
    onAgent('agent/inbox/discarded', ({ message }) => this.onInbox(message, 'discarded'))
    const sessionDisposer = this.ctx.on('session/event', (subject, event) => {
      if (this.closed || subject.id !== this.rootSessionId) return
      if (event.type === 'user/message') this.onUserMessage(event.data)
      else if (event.type === 'turn/end') this.onTurnEnd(event.data)
    })
    if (typeof sessionDisposer === 'function') this.disposers.push(sessionDisposer)
    // A rejected step ends the claimed message without a `discarded` event.
    // Observe the waterfall without changing the decision.
    const preStepDisposer = this.ctx.on('agent/pre-step', async (payload, next) => {
      const decision = await next()
      if (
        !this.closed &&
        payload?.agent?.id === this.rootSessionId &&
        decision?.kind === 'reject'
      ) {
        for (const message of payload.messages ?? []) {
          this.failDelivery(message?.id, '步骤被拒绝（pre-step reject）；模型不会收到')
        }
      }
      return decision
    })
    if (typeof preStepDisposer === 'function') this.disposers.push(preStepDisposer)
  }

  /** Stop publishing and clear every timer; observed jobs are untouched. */
  close() {
    if (this.closed) return
    this.closed = true
    for (const timer of this.publishTimers.values()) this.clearTimer(timer)
    this.publishTimers.clear()
    for (const monitor of this.monitors.values()) this.clearMonitorTimers(monitor)
    this.deliveries.clear()
    for (const disposer of this.disposers.splice(0)) {
      try {
        disposer()
      } catch {
        // listener disposal is best-effort during teardown
      }
    }
  }

  /**
   * Attach one monitor to one visible job.
   * @param {object} params - address, jobId, from, mode, filter, budgets.
   * @returns {{existing: boolean, monitor: object}} projection.
   */
  create(params) {
    this.assertAvailable()
    this.assertAddress(params?.address)
    const jobId = validateJobId(params.jobId)
    const from = params.from ?? 'new'
    if (from !== 'new' && from !== 'earliest') {
      throw new RpcError('jsonrpc/invalid-params', 'from must be new or earliest')
    }
    const mode = params.mode ?? 'notify'
    if (mode !== 'notify' && mode !== 'quiet') {
      throw new RpcError('jsonrpc/invalid-params', 'mode must be notify or quiet')
    }
    const filter = parseFilter(params.filter)
    const budgets = parseBudgets(params.budgets)

    const existing = this.monitors.get(jobId)
    if (existing !== undefined && existing.listening && !existing.exhausted) {
      return { existing: true, monitor: this.project(existing) }
    }
    if (existing !== undefined) {
      // An inactive record is replaced by a fresh listening generation. Drop
      // its delivery map entries so a late receipt cannot resurrect it.
      this.clearMonitorTimers(existing)
      for (const [messageId, entry] of this.deliveries) {
        if (entry.monitor === existing) this.deliveries.delete(messageId)
      }
    }
    if (existing === undefined && this.monitors.size >= MONITOR_LIMITS.maxMonitors) {
      throw new RpcError('nano/monitor/limit-reached', `at most ${MONITOR_LIMITS.maxMonitors} monitors are kept`)
    }

    let job
    try {
      job = this.registry.get(jobId, this.rootSessionId)
    } catch (error) {
      throw mapRegistryError('read job before monitoring', error)
    }
    const total = safeUint(job?.output?.total)
    const earliest = safeUint(job?.output?.earliest)
    const unreadBytes = Math.max(0, total - earliest)
    const initialOffset = from === 'earliest' ? earliest : total
    const now = this.timers.now()
    const monitor = {
      id: jobId,
      jobId,
      mode,
      listening: true,
      from,
      initialOffset,
      offset: initialOffset,
      unreadBytes,
      skippedBytes: from === 'new' ? unreadBytes : 0,
      filter,
      mergeWindowMs: budgets.mergeWindowMs,
      budgets: {
        maxNotifications: budgets.maxNotifications,
        minIntervalMs: budgets.minIntervalMs,
        maxLinesPerNotification: budgets.maxLinesPerNotification,
        maxNotificationBytes: budgets.maxNotificationBytes,
        maxNotificationTokens: budgets.maxNotificationTokens,
        expiresAt: now + budgets.expiresInMs,
      },
      expiresInMs: budgets.expiresInMs,
      used: { notifications: 0, lines: 0, bytes: 0, estimatedTokens: 0 },
      exhausted: false,
      exhaustReason: null,
      stream: 'watching',
      streamDetail: null,
      jobStatus: String(job?.status ?? 'running'),
      lossy: false,
      gaps: 0,
      heldMatches: 0,
      heldBytes: 0,
      droppedMatches: 0,
      lastError: null,
      deliveries: [],
      role: 'user',
      source: 'monitor',
      createdAt: now,
      updatedAt: now,
      // Internal state, never projected.
      pending: null,
      ready: [],
      held: null,
      paused: false,
      carry: { text: '', offset: initialOffset, partial: false },
      gapBeforeNext: false,
      reading: false,
      dirty: false,
      stoppedByUser: false,
      settled: false,
      lastDeliveryAt: 0,
      flushTimer: undefined,
      expiryTimer: undefined,
      deliveryTimers: new Map(),
    }
    this.monitors.set(jobId, monitor)
    this.scheduleExpiry(monitor)

    // `earliest` replays the retained head immediately, even while the job is
    // still running. A settled job has no later `settled` event to wait for,
    // so it also needs the final carry flushed now.
    const status = job?.status
    const terminal = status === 'completed' || status === 'killed' || status === 'failed'
    if (from === 'earliest' || terminal) this.drain(monitor)
    if (terminal) {
      this.flushCarry(monitor)
      this.markStreamFromJob(monitor, job)
    }
    this.scheduleFlush(monitor, true)
    this.publish(monitor)
    return { existing: false, monitor: this.project(monitor) }
  }

  /**
   * List every monitor in this host generation.
   * @param {object} params - `address`.
   * @returns {{seq: number, monitors: object[]}} projections.
   */
  list(params) {
    this.assertAvailable()
    this.assertAddress(params?.address)
    return {
      seq: this.eventSeq,
      monitors: [...this.monitors.values()].map((monitor) => this.project(monitor)),
    }
  }

  /**
   * Stop monitor delivery only. The observed job and its output are untouched.
   * @param {object} params - address, jobId.
   * @returns {{monitor: object}} projection.
   */
  stop(params) {
    this.assertAvailable()
    this.assertAddress(params?.address)
    const monitor = this.find(params.jobId)
    if (monitor === undefined) {
      throw new RpcError('nano/monitor/not-found', 'no monitor is attached to this job')
    }
    if (!monitor.stoppedByUser) {
      monitor.stoppedByUser = true
      monitor.listening = false
      monitor.droppedMatches += countWindows(monitor)
      monitor.pending = null
      monitor.ready = []
      monitor.held = null
      monitor.carry = { text: '', offset: monitor.offset, partial: false }
      monitor.heldMatches = 0
      monitor.heldBytes = 0
      monitor.paused = false
      this.clearFlushTimer(monitor)
      this.clearExpiryTimer(monitor)
      monitor.updatedAt = this.timers.now()
      this.publish(monitor)
    }
    return { monitor: this.project(monitor) }
  }

  /**
   * Reset the notification count and extend validity by explicit user action.
   * External text can never reach this path.
   * @param {object} params - address, jobId, optional budgets.
   * @returns {{monitor: object}} projection.
   */
  resetBudget(params) {
    this.assertAvailable()
    this.assertAddress(params?.address)
    const monitor = this.find(params.jobId)
    if (monitor === undefined) {
      throw new RpcError('nano/monitor/not-found', 'no monitor is attached to this job')
    }
    const budgets = parseBudgets(params.budgets, monitor)
    monitor.mergeWindowMs = budgets.mergeWindowMs
    monitor.budgets.maxNotifications = budgets.maxNotifications
    monitor.budgets.minIntervalMs = budgets.minIntervalMs
    monitor.budgets.maxLinesPerNotification = budgets.maxLinesPerNotification
    monitor.budgets.maxNotificationBytes = budgets.maxNotificationBytes
    monitor.budgets.maxNotificationTokens = budgets.maxNotificationTokens
    monitor.expiresInMs = budgets.expiresInMs
    monitor.budgets.expiresAt = this.timers.now() + budgets.expiresInMs
    monitor.used = { notifications: 0, lines: 0, bytes: 0, estimatedTokens: 0 }
    monitor.exhausted = false
    monitor.exhaustReason = null
    if (monitor.held !== null) {
      // The held summary is the next window to deliver after an explicit reset.
      monitor.pending = monitor.held
      monitor.held = null
    }
    monitor.heldMatches = 0
    monitor.heldBytes = 0
    monitor.paused = false
    monitor.listening = monitor.stream === 'watching' && !monitor.stoppedByUser
    monitor.updatedAt = this.timers.now()
    this.scheduleExpiry(monitor)
    if ((monitor.pending !== null || monitor.ready.length > 0) && this.canDeliver(monitor)) {
      this.scheduleFlush(monitor, true)
    }
    if (monitor.dirty) {
      monitor.dirty = false
      this.scheduleDrain(monitor)
    }
    this.publish(monitor)
    return { monitor: this.project(monitor) }
  }

  /** Apply one official job event for a monitored job. */
  onJobEvent(event) {
    if (this.closed) return
    const jobId = String(
      event.type === 'output' ? event.id : event.job?.id ?? '',
    )
    const monitor = this.monitors.get(jobId)
    if (monitor === undefined) return
    if (event.type === 'output') {
      this.scheduleDrain(monitor)
      return
    }
    if (event.job !== undefined) {
      monitor.jobStatus = String(event.job.status ?? monitor.jobStatus)
    }
    if (event.type === 'settled') {
      // The official pump has already drained the source before settlement.
      // One read at our own offset catches the final output; waiting for a
      // post-settlement output frame would never arrive.
      this.drain(monitor)
      this.flushCarry(monitor)
      this.markStreamFromJob(monitor, event.job)
      this.scheduleFlush(monitor, true)
      this.publish(monitor)
      return
    }
    if (event.type === 'removed') {
      this.flushCarry(monitor)
      this.markStream(monitor, 'stream-ended', '任务记录已从官方列表移除')
      this.scheduleFlush(monitor, true)
      this.publish(monitor)
      return
    }
    this.schedulePublish(monitor)
  }

  /** Schedule one bounded, non-consuming read for a monitor. */
  scheduleDrain(monitor) {
    if (monitor.settled || monitor.stoppedByUser) return
    if (monitor.paused) {
      // The ready queue is full; keep the ring from growing the in-memory
      // projection. The dirty bit resumes the drain after a delivery.
      monitor.dirty = true
      return
    }
    if (monitor.reading) {
      monitor.dirty = true
      return
    }
    this.drain(monitor)
  }

  /** Read every retained chunk from the monitor's own offset. */
  drain(monitor) {
    if (this.closed || monitor.reading || this.registry === undefined) return
    monitor.reading = true
    try {
      do {
        monitor.dirty = false
        const read = this.registry.readAt(monitor.jobId, monitor.offset, this.rootSessionId)
        if (read.lossy === true) {
          monitor.lossy = true
          this.markGap(monitor)
        }
        const chunks = read.chunks ?? []
        if (
          chunks.length === 0 &&
          typeof read.next === 'number' &&
          read.next > monitor.offset &&
          read.lossy !== true
        ) {
          // The provider advanced without returning bytes. Never jump the
          // cursor silently; show the discontinuity like a lossy read.
          monitor.lossy = true
          this.markGap(monitor)
        }
        for (const chunk of chunks) {
          this.consumeChunk(monitor, chunk)
        }
        if (typeof read.next === 'number' && read.next > monitor.offset) {
          monitor.offset = read.next
        }
      } while (monitor.dirty && !this.closed && !monitor.paused)
      if (monitor.ready.length >= MONITOR_LIMITS.maxReadyWindows) monitor.paused = true
      monitor.updatedAt = this.timers.now()
      this.schedulePublish(monitor)
    } catch (error) {
      monitor.lastError = describe(error)
      this.publish(monitor)
    } finally {
      monitor.reading = false
    }
  }

  /** Consume one retained chunk into the line assembler. */
  consumeChunk(monitor, chunk) {
    if (chunk === null || typeof chunk !== 'object') return
    if (chunk.gapBefore === true) this.markGap(monitor)
    let text = typeof chunk.text === 'string' ? chunk.text : ''
    if (text === '') return
    let at = safeUint(chunk.at)
    const carryBytes = byteLength(monitor.carry.text)
    const expected =
      monitor.carry.text === '' ? monitor.carry.offset : monitor.carry.offset + carryBytes
    if (at < expected) {
      // A read starting inside a retained chunk returns the whole chunk; skip
      // the bytes this monitor already processed.
      text = sliceUtf8(text, expected - at)
      at = expected
    }
    if (monitor.carry.text === '') {
      if (at !== expected && !monitor.gapBeforeNext) {
        // The stream skipped bytes without an explicit lossy/gap flag. Never
        // treat the next line as a continuation of the missing range.
        this.markGap(monitor)
      }
      monitor.carry.offset = at
      monitor.gapBeforeNext = false
    } else if (at !== expected) {
      // The next chunk does not continue the partial line. Never splice them.
      this.markGap(monitor)
      monitor.carry.offset = at
      monitor.gapBeforeNext = false
    }
    monitor.carry.text += text
    this.extractLines(monitor)
  }

  /** Split complete lines; a long line is emitted as a marked block. */
  extractLines(monitor) {
    while (!this.closed) {
      const carry = monitor.carry
      const newline = carry.text.indexOf('\n')
      if (newline >= 0) {
        let line = carry.text.slice(0, newline)
        const consumed = byteLength(line) + 1
        const offset = carry.offset
        carry.offset += consumed
        carry.text = carry.text.slice(newline + 1)
        carry.partial = false
        if (line.endsWith('\r')) line = line.slice(0, -1)
        this.emitLine(monitor, line, offset, false)
        continue
      }
      if (byteLength(carry.text) <= MONITOR_LIMITS.maxLineBytes) break
      // No newline yet: process one bounded block instead of treating half a
      // line as a complete line or growing memory without bound.
      const block = truncateUtf8(carry.text, MONITOR_LIMITS.maxLineBytes)
      const consumed = byteLength(block)
      if (consumed === 0) break
      const offset = carry.offset
      carry.offset += consumed
      carry.text = sliceUtf8(carry.text, consumed)
      carry.partial = true
      this.emitLine(monitor, block, offset, true)
    }
  }

  /** Emit the final line when the observed stream ends without a newline. */
  flushCarry(monitor) {
    if (this.closed) return
    const carry = monitor.carry
    if (carry.text === '') return
    const text = carry.text
    const offset = carry.offset
    const partial = carry.partial === true
    carry.offset += byteLength(text)
    carry.text = ''
    carry.partial = false
    this.emitLine(monitor, text, offset, partial)
  }

  /** Apply the filter and merge one line event into the pending notification. */
  emitLine(monitor, text, offset, partial) {
    if (!this.matches(monitor, text)) return
    if (monitor.stoppedByUser) {
      // The user stopped delivery. A settlement may still drain the final
      // bytes; count those matches as dropped, never as held for a resume.
      monitor.droppedMatches += 1
      this.schedulePublish(monitor)
      return
    }
    if (monitor.exhausted) {
      // Keep the bounded summary visible instead of silently dropping the
      // match. The UI reports how many matching lines are held.
      this.holdLine(monitor, text, offset, partial)
      this.schedulePublish(monitor)
      return
    }
    if (monitor.pending === null) {
      monitor.pending = {
        first: { text, offset, partial },
        last: { text, offset, partial },
        count: 1,
        startedAt: this.timers.now(),
      }
    } else {
      monitor.pending.last = { text, offset, partial }
      monitor.pending.count += 1
    }
    if (monitor.pending.count >= monitor.budgets.maxLinesPerNotification) {
      // A full window is a complete notification. Close it now so no single
      // notification ever covers more than maxLinesPerNotification line events.
      monitor.ready.push(monitor.pending)
      monitor.pending = null
      if (monitor.ready.length >= MONITOR_LIMITS.maxReadyWindows) monitor.paused = true
      this.scheduleFlush(monitor, true)
    } else {
      this.scheduleFlush(monitor, false)
    }
  }

  /** Record one matching line in the bounded post-exhaustion summary. */
  holdLine(monitor, text, offset, partial) {
    if (monitor.held === null) {
      monitor.held = {
        first: { text, offset, partial },
        last: { text, offset, partial },
        count: 1,
        startedAt: this.timers.now(),
      }
    } else {
      monitor.held.last = { text, offset, partial }
      monitor.held.count += 1
    }
    monitor.heldMatches = monitor.held.count
    monitor.heldBytes =
      byteLength(monitor.held.first.text) + byteLength(monitor.held.last.text)
  }

  /** Merge every queued window into the held summary after exhaustion. */
  holdAll(monitor) {
    const windows = [...monitor.ready]
    if (monitor.pending !== null) windows.push(monitor.pending)
    const existing = monitor.held
    monitor.ready = []
    monitor.pending = null
    monitor.held = null
    monitor.paused = false
    for (const window of windows) monitor.held = mergeWindow(monitor.held, window)
    if (existing !== null) monitor.held = mergeWindow(monitor.held, existing)
    monitor.heldMatches = monitor.held?.count ?? 0
    monitor.heldBytes =
      monitor.held === null
        ? 0
        : byteLength(monitor.held.first.text) + byteLength(monitor.held.last.text)
  }

  /** Schedule the merge-window flush, respecting the minimum interval. */
  scheduleFlush(monitor, immediate) {
    if (this.closed || !this.canDeliver(monitor)) return
    if (monitor.ready.length === 0 && monitor.pending === null) return
    const now = this.timers.now()
    const interval = monitor.budgets.minIntervalMs
    const intervalWait = Math.max(0, monitor.lastDeliveryAt + interval - now)
    // A full window only waits for the minimum interval; an open window also
    // waits for the merge window.
    const delay =
      immediate || monitor.ready.length > 0
        ? intervalWait
        : Math.max(monitor.mergeWindowMs, intervalWait)
    if (monitor.flushTimer !== undefined) {
      this.clearTimer(monitor.flushTimer)
      monitor.flushTimer = undefined
    }
    monitor.flushTimer = this.timers.setTimeout(() => {
      monitor.flushTimer = undefined
      this.flush(monitor)
    }, delay)
  }

  /** Deliver one merged line notification through the official Agent surface. */
  flush(monitor) {
    if (this.closed || !this.canDeliver(monitor)) return
    const window =
      monitor.ready.length > 0 ? monitor.ready.shift() : monitor.pending
    if (window === null || window === undefined) return
    if (window === monitor.pending) monitor.pending = null
    const text = buildNotification(monitor, window)
    const bytes = byteLength(text)
    const tokens = estimateTokens(text)
    const messageId = randomUUID()
    const delivery = {
      messageId,
      mode: monitor.mode,
      state: 'submitted',
      at: this.timers.now(),
      lines: window.count,
      bytes,
      estimatedTokens: tokens,
      firstOffset: window.first.offset,
      lastOffset: window.last.offset,
      omittedLines: window.count > 2 ? window.count - 2 : 0,
      textPreview: boundedText(text, 240),
      detail: null,
    }
    monitor.used.notifications += 1
    monitor.used.lines += delivery.lines
    monitor.used.bytes += bytes
    monitor.used.estimatedTokens += tokens
    monitor.lastDeliveryAt = delivery.at
    monitor.deliveries.push(delivery)
    while (monitor.deliveries.length > MONITOR_LIMITS.maxDeliveriesKept) {
      monitor.deliveries.shift()
    }
    this.deliveries.set(messageId, { monitor, delivery })
    while (this.deliveries.size > MONITOR_LIMITS.maxDeliveryEntries) {
      const oldest = this.deliveries.keys().next().value
      this.deliveries.delete(oldest)
    }

    const message = Object.freeze({
      id: messageId,
      role: 'user',
      content: Object.freeze([Object.freeze({ type: 'text', text })]),
      source: Object.freeze({ kind: 'monitor' }),
    })
    try {
      if (monitor.mode === 'notify') this.agent.followup(message)
      else this.agent.inject(message)
    } catch (error) {
      delivery.state = 'failed'
      delivery.detail = `投递调用失败：${describe(error)}`
    }
    if (delivery.state === 'submitted') this.scheduleDeliveryTimeout(monitor, delivery)
    if (monitor.used.notifications >= monitor.budgets.maxNotifications) {
      this.exhaust(monitor, 'notifications')
    }
    if (monitor.paused && monitor.ready.length < MONITOR_LIMITS.maxReadyWindows) {
      monitor.paused = false
      if (monitor.dirty) {
        monitor.dirty = false
        this.scheduleDrain(monitor)
      }
    }
    if (!monitor.exhausted && monitor.ready.length > 0) {
      this.scheduleFlush(monitor, true)
    } else if (!monitor.exhausted && monitor.pending !== null) {
      this.scheduleFlush(monitor, false)
    }
    monitor.updatedAt = this.timers.now()
    this.publish(monitor)
  }

  /** Mark a monitor exhausted; later matches stay in a bounded held summary. */
  exhaust(monitor, reason) {
    monitor.exhausted = true
    monitor.exhaustReason = reason
    monitor.listening = false
    this.holdAll(monitor)
    this.clearFlushTimer(monitor)
    this.clearExpiryTimer(monitor)
    monitor.updatedAt = this.timers.now()
  }

  /** Update the stream fact from an official terminal projection. */
  markStreamFromJob(monitor, job) {
    const status = String(job?.status ?? monitor.jobStatus)
    monitor.jobStatus = status
    const detail = typeof job?.detail === 'string' ? job.detail : null
    if (status === 'completed') this.markStream(monitor, 'stream-ended', detail)
    else if (status === 'killed') this.markStream(monitor, 'stopped', detail)
    else if (status === 'failed') this.markStream(monitor, 'failed', detail)
    else this.markStream(monitor, 'stream-ended', detail)
  }

  /** Set the UI-only stream fact. This never creates a model notice. */
  markStream(monitor, stream, detail) {
    monitor.settled = true
    monitor.stream = stream
    monitor.streamDetail = detail
    monitor.listening = false
    this.clearExpiryTimer(monitor)
  }

  /** Record a discontinuity without splicing text across it. */
  markGap(monitor) {
    monitor.gaps += 1
    monitor.carry = { text: '', offset: monitor.offset, partial: false }
    monitor.gapBeforeNext = true
    this.schedulePublish(monitor)
  }

  /** Apply one official inbox receipt for a monitor message. */
  onInbox(message, state, turn) {
    if (message?.id === undefined) return
    const entry = this.deliveries.get(message.id)
    if (entry === undefined) return
    const { monitor, delivery } = entry
    if (delivery.state === 'accepted' || delivery.state === 'failed' || delivery.state === 'discarded') return
    if (state === 'discarded') {
      delivery.state = 'discarded'
      delivery.detail = '官方在步骤前丢弃了该消息；模型不会收到'
    } else if (state === 'claimed') {
      if (typeof turn === 'number') delivery.turn = turn
      if (delivery.state === 'claimed') return
      delivery.state = 'claimed'
      delivery.detail = '已交给步骤批次；步骤仍可能被拒绝，尚不证明模型收到'
    } else if (state === 'inserted') {
      // A late inserted event must not downgrade a claimed delivery.
      if (delivery.state !== 'submitted' && delivery.state !== 'inserted') return
      delivery.state = 'inserted'
      delivery.detail = monitor.mode === 'quiet'
        ? '已入队；quiet 不唤醒，空闲时可能一直等待'
        : '已入队；等待被步骤接收'
    }
    monitor.updatedAt = this.timers.now()
    this.publish(monitor)
  }

  /** A durable user/message with the delivery id is the official receipt. */
  onUserMessage(data) {
    if (data?.id === undefined) return
    const entry = this.deliveries.get(data.id)
    if (entry === undefined) return
    const { monitor, delivery } = entry
    if (delivery.state === 'accepted') return
    delivery.state = 'accepted'
    delivery.detail = 'durable user/message：官方记录已接受该用户回合'
    this.clearDeliveryTimer(monitor, delivery.messageId)
    monitor.updatedAt = this.timers.now()
    this.publish(monitor)
  }

  /** A blocked turn means claimed messages never reached the model. */
  onTurnEnd(data) {
    if (data?.reason?.kind !== 'blocked' || typeof data.turn !== 'number') return
    for (const entry of this.deliveries.values()) {
      const { delivery } = entry
      if (delivery.turn !== data.turn) continue
      this.failDelivery(
        delivery.messageId,
        '回合被阻断（blocked）；模型没有收到该消息',
      )
    }
  }

  /** Mark one delivery as a definite failure without inventing a receipt. */
  failDelivery(messageId, detail) {
    if (typeof messageId !== 'string') return
    const entry = this.deliveries.get(messageId)
    if (entry === undefined) return
    const { monitor, delivery } = entry
    if (delivery.state === 'accepted' || delivery.state === 'failed' || delivery.state === 'discarded') return
    delivery.state = 'failed'
    delivery.detail = detail
    this.clearDeliveryTimer(monitor, messageId)
    monitor.updatedAt = this.timers.now()
    this.publish(monitor)
  }

  /** Schedule the honest "unknown" state when no official receipt arrives. */
  scheduleDeliveryTimeout(monitor, delivery) {
    const timer = this.timers.setTimeout(() => {
      monitor.deliveryTimers.delete(delivery.messageId)
      if (delivery.state === 'accepted' || delivery.state === 'discarded' || delivery.state === 'failed') return
      delivery.state = 'unknown'
      delivery.detail = '未观察到被接受的步骤或 durable user/message；不能显示为模型已收到'
      monitor.updatedAt = this.timers.now()
      this.publish(monitor)
    }, MONITOR_LIMITS.deliveryTimeoutMs)
    monitor.deliveryTimers.set(delivery.messageId, timer)
  }

  scheduleExpiry(monitor) {
    this.clearExpiryTimer(monitor)
    const delay = Math.max(0, monitor.budgets.expiresAt - this.timers.now())
    monitor.expiryTimer = this.timers.setTimeout(() => {
      monitor.expiryTimer = undefined
      if (this.closed || monitor.settled || monitor.exhausted) return
      this.exhaust(monitor, 'expired')
      this.publish(monitor)
    }, delay)
  }

  /** Coalesce live projection updates instead of publishing per chunk. */
  schedulePublish(monitor) {
    if (this.closed) return
    if (this.publishTimers.has(monitor.jobId)) return
    const timer = this.timers.setTimeout(() => {
      this.publishTimers.delete(monitor.jobId)
      if (!this.closed) this.publish(monitor)
    }, MONITOR_LIMITS.publishThrottleMs)
    this.publishTimers.set(monitor.jobId, timer)
  }

  /** Publish the full monitor projection (absolute replacement). */
  publish(monitor) {
    if (this.closed) return
    // A replaced generation must never resurrect its projection over the
    // current monitor for the same job.
    if (this.monitors.get(monitor.jobId) !== monitor) return
    const scheduled = this.publishTimers.get(monitor.jobId)
    if (scheduled !== undefined) {
      this.clearTimer(scheduled)
      this.publishTimers.delete(monitor.jobId)
    }
    this.eventSeq += 1
    this.hub.publish('monitor.updated', {
      address: this.address,
      seq: this.eventSeq,
      monitor: this.project(monitor),
    })
  }

  /** Wire projection; internal carry, timers, and maps never cross the bridge. */
  project(monitor) {
    return {
      id: monitor.id,
      jobId: monitor.jobId,
      mode: monitor.mode,
      listening: monitor.listening,
      from: monitor.from,
      initialOffset: monitor.initialOffset,
      offset: monitor.offset,
      unreadBytes: monitor.unreadBytes,
      skippedBytes: monitor.skippedBytes,
      filter: monitor.filter === null ? null : { pattern: monitor.filter.pattern, regex: monitor.filter.regex },
      mergeWindowMs: monitor.mergeWindowMs,
      budgets: { ...monitor.budgets },
      used: { ...monitor.used },
      exhausted: monitor.exhausted,
      exhaustReason: monitor.exhaustReason,
      stoppedByUser: monitor.stoppedByUser,
      stream: monitor.stream,
      streamDetail: monitor.streamDetail,
      jobStatus: monitor.jobStatus,
      lossy: monitor.lossy,
      gaps: monitor.gaps,
      heldMatches: monitor.heldMatches,
      heldBytes: monitor.heldBytes,
      droppedMatches: monitor.droppedMatches,
      lastError: monitor.lastError,
      deliveries: monitor.deliveries.map((delivery) => ({ ...delivery })),
      role: monitor.role,
      source: monitor.source,
      createdAt: monitor.createdAt,
      updatedAt: monitor.updatedAt,
    }
  }

  canDeliver(monitor) {
    return !this.closed && !monitor.stoppedByUser && !monitor.exhausted
  }

  matches(monitor, text) {
    if (monitor.filter === null) return true
    if (monitor.filter.regex) {
      try {
        return monitor.filter.expression.test(text)
      } catch {
        return false
      }
    }
    return text.includes(monitor.filter.pattern)
  }

  find(jobId) {
    if (typeof jobId !== 'string' || jobId === '') {
      throw new RpcError('jsonrpc/invalid-params', 'jobId must be a non-empty string')
    }
    return this.monitors.get(jobId)
  }

  assertAvailable() {
    if (
      this.registry === undefined ||
      typeof this.registry.readAt !== 'function' ||
      !this.eventsAvailable
    ) {
      throw new RpcError(
        'nano/monitor/unavailable',
        'the official job registry does not expose live, non-consuming output observation',
      )
    }
    if (this.agent === undefined || typeof this.agent.followup !== 'function' || typeof this.agent.inject !== 'function') {
      throw new RpcError('nano/monitor/unavailable', 'the official Agent delivery surface is not available')
    }
  }

  /** Only the exact root address is served. */
  assertAddress(address) {
    if (address === null || typeof address !== 'object' || Array.isArray(address)) {
      throw new RpcError('jsonrpc/invalid-params', 'address must be an object')
    }
    if (typeof address.rootSessionId !== 'string' || address.rootSessionId === '') {
      throw new RpcError('jsonrpc/invalid-params', 'address.rootSessionId must be a non-empty string')
    }
    if (address.rootSessionId !== this.rootSessionId) {
      throw new RpcError('nano/conversation/address', 'address names a root session this host does not serve')
    }
    if (address.childSessionId !== undefined) {
      throw new RpcError('nano/unsupported', 'child-session addresses belong to S12 and are not served by this generation')
    }
  }

  clearMonitorTimers(monitor) {
    this.clearFlushTimer(monitor)
    this.clearExpiryTimer(monitor)
    for (const timer of monitor.deliveryTimers.values()) this.clearTimer(timer)
    monitor.deliveryTimers.clear()
  }

  clearFlushTimer(monitor) {
    if (monitor.flushTimer !== undefined) {
      this.clearTimer(monitor.flushTimer)
      monitor.flushTimer = undefined
    }
  }

  clearExpiryTimer(monitor) {
    if (monitor.expiryTimer !== undefined) {
      this.clearTimer(monitor.expiryTimer)
      monitor.expiryTimer = undefined
    }
  }

  clearDeliveryTimer(monitor, messageId) {
    const timer = monitor.deliveryTimers.get(messageId)
    if (timer !== undefined) {
      this.clearTimer(timer)
      monitor.deliveryTimers.delete(messageId)
    }
  }

  clearTimer(timer) {
    try {
      this.timers.clearTimeout(timer)
    } catch {
      // timer already fired
    }
  }
}

/** Validate and compile one optional literal/regex line filter. */
function parseFilter(value) {
  if (value === undefined || value === null) return null
  if (typeof value !== 'object' || Array.isArray(value)) {
    throw new RpcError('jsonrpc/invalid-params', 'filter must be an object')
  }
  const pattern = value.pattern
  if (typeof pattern !== 'string' || pattern === '') {
    throw new RpcError('nano/monitor/filter-invalid', 'filter.pattern must be a non-empty string')
  }
  const bytes = byteLength(pattern)
  if (bytes > MONITOR_LIMITS.maxFilterBytes) {
    throw new RpcError(
      'nano/monitor/filter-invalid',
      `filter.pattern is ${bytes} bytes, over the ${MONITOR_LIMITS.maxFilterBytes} byte limit`,
    )
  }
  const regex = value.regex === true
  const filter = { pattern, regex }
  if (regex) {
    const rejected = regexComplexity(pattern)
    if (rejected !== null) {
      throw new RpcError('nano/monitor/filter-invalid', rejected)
    }
    try {
      filter.expression = new RegExp(pattern)
    } catch (error) {
      throw new RpcError('nano/monitor/filter-invalid', `invalid regular expression: ${describe(error)}`)
    }
  }
  return filter
}

/** A bounded heuristic for patterns with catastrophic backtracking risk. */
function regexComplexity(pattern) {
  if (/\\[1-9]/.test(pattern)) {
    return 'backreferences are not allowed in monitor filters'
  }
  // A quantified group whose body already contains a quantifier, e.g. (a+)+
  // or (a*){2,}. This is the common exponential-backtracking shape.
  if (/\((?:[^()\\]|\\.)*[+*](?:[^()\\]|\\.)*\)\s*(?:[+*?]|\{\d*,?\d*\})/.test(pattern)) {
    return 'nested quantifiers are not allowed in monitor filters'
  }
  const quantifiers = (pattern.match(/[*+?{]/g) ?? []).length
  if (quantifiers > 64) {
    return 'monitor filter has too many quantifier tokens'
  }
  return null
}

/** Validate per-monitor budgets, bounded by hard limits. */
function parseBudgets(value, base) {
  const source = value === undefined || value === null ? {} : value
  if (typeof source !== 'object' || Array.isArray(source)) {
    throw new RpcError('nano/monitor/budget-invalid', 'budgets must be an object')
  }
  const fallback = base?.budgets ?? MONITOR_DEFAULTS
  const mergeWindowMs = boundedInteger(
    source.mergeWindowMs,
    base?.mergeWindowMs ?? MONITOR_DEFAULTS.mergeWindowMs,
    'mergeWindowMs',
    MONITOR_LIMITS.minMergeWindowMs,
    MONITOR_LIMITS.maxMergeWindowMs,
  )
  const maxNotifications = boundedInteger(
    source.maxNotifications,
    fallback.maxNotifications,
    'maxNotifications',
    MONITOR_LIMITS.minNotifications,
    MONITOR_LIMITS.maxNotifications,
  )
  const minIntervalMs = boundedInteger(
    source.minIntervalMs,
    fallback.minIntervalMs,
    'minIntervalMs',
    MONITOR_LIMITS.minIntervalMs,
    MONITOR_LIMITS.maxIntervalMs,
  )
  const maxLinesPerNotification = boundedInteger(
    source.maxLinesPerNotification,
    fallback.maxLinesPerNotification,
    'maxLinesPerNotification',
    MONITOR_LIMITS.minLines,
    MONITOR_LIMITS.maxLines,
  )
  const maxNotificationBytes = boundedInteger(
    source.maxNotificationBytes,
    fallback.maxNotificationBytes,
    'maxNotificationBytes',
    MONITOR_LIMITS.minBytes,
    MONITOR_LIMITS.maxBytes,
  )
  const maxNotificationTokens = boundedInteger(
    source.maxNotificationTokens,
    fallback.maxNotificationTokens,
    'maxNotificationTokens',
    MONITOR_LIMITS.minTokens,
    MONITOR_LIMITS.maxTokens,
  )
  const expiresInMs = boundedInteger(
    source.expiresInMs,
    base?.expiresInMs ?? MONITOR_DEFAULTS.expiresInMs,
    'expiresInMs',
    MONITOR_LIMITS.minExpiresMs,
    MONITOR_LIMITS.maxExpiresMs,
  )
  return {
    mergeWindowMs,
    maxNotifications,
    minIntervalMs,
    maxLinesPerNotification,
    maxNotificationBytes,
    maxNotificationTokens,
    expiresInMs,
  }
}

function boundedInteger(value, fallback, field, min, max) {
  if (value === undefined) return fallback
  if (!Number.isSafeInteger(value) || value < min || value > max) {
    throw new RpcError('nano/monitor/budget-invalid', `${field} must be an integer in [${min}, ${max}]`)
  }
  return value
}

/** Build the bounded line-event notification. Original line text is kept
 * unless the per-notification byte/token budget forces an explicit truncation. */
export function buildNotification(monitor, pending) {
  const maxBytes = Math.min(
    monitor.budgets.maxNotificationBytes,
    monitor.budgets.maxNotificationTokens * 4,
  )
  // Job ids are bounded at the RPC boundary, but truncate the display copy so
  // a pathological id cannot push the skeleton past a tiny notification budget.
  const jobLabel = truncateUtf8(monitor.jobId, 128)
  const header = `[monitor] job=${jobLabel} matched=${pending.count}`
  const omitted = pending.count > 2 ? pending.count - 2 : 0
  const firstLabel = pending.first.partial ? ' (long-line block)' : ''
  const lastLabel = pending.last.partial ? ' (long-line block)' : ''
  const footer = `omitted=${omitted}`
  const skeleton = `${header}\nfirst@${pending.first.offset}${firstLabel}: \nlast@${pending.last.offset}${lastLabel}: \n${footer}`
  const available = Math.max(1, maxBytes - byteLength(skeleton))
  let firstText
  let lastText
  if (pending.count === 1) {
    firstText = boundedText(pending.first.text, Math.max(1, Math.floor(available / 2)))
    lastText = firstText
  } else {
    firstText = boundedText(pending.first.text, Math.max(1, Math.floor(available / 2)))
    lastText = boundedText(
      pending.last.text,
      Math.max(1, available - byteLength(firstText)),
    )
  }
  return `${header}\nfirst@${pending.first.offset}${firstLabel}: ${firstText}\nlast@${pending.last.offset}${lastLabel}: ${lastText}\n${footer}`
}

/** Merge two first/last windows in chronological order. */
function mergeWindow(first, second) {
  if (first === null || first === undefined) {
    return {
      first: second.first,
      last: second.last,
      count: second.count,
      startedAt: second.startedAt,
    }
  }
  return {
    first: first.first,
    last: second.last,
    count: first.count + second.count,
    startedAt: first.startedAt,
  }
}

/** How many matching line events are not yet delivered. */
function countWindows(monitor) {
  let count = monitor.pending?.count ?? 0
  for (const window of monitor.ready) count += window.count
  if (monitor.held !== null) count += monitor.held.count
  return count
}

function estimateTokens(text) {
  return Math.max(1, Math.ceil(byteLength(text) / 4))
}

function safeUint(value) {
  return Number.isSafeInteger(value) && value >= 0 ? value : 0
}

function byteLength(text) {
  return Buffer.byteLength(typeof text === 'string' ? text : '', 'utf8')
}

/** Truncate a line for the notification and mark the cut explicitly. */
function boundedText(text, maxBytes) {
  const value = typeof text === 'string' ? text : ''
  if (byteLength(value) <= maxBytes) return value
  const marker = '…[truncated]'
  const room = Math.max(0, maxBytes - byteLength(marker))
  return `${truncateUtf8(value, room)}${marker}`
}

function truncateUtf8(text, maxBytes) {
  const value = typeof text === 'string' ? text : ''
  if (byteLength(value) <= maxBytes) return value
  const buffer = Buffer.from(value, 'utf8')
  let end = Math.max(0, Math.min(buffer.length, maxBytes))
  while (end > 0 && (buffer[end] & 0xc0) === 0x80) end -= 1
  return buffer.subarray(0, end).toString('utf8')
}

function sliceUtf8(text, byteOffset) {
  const buffer = Buffer.from(typeof text === 'string' ? text : '', 'utf8')
  if (byteOffset <= 0) return buffer.toString('utf8')
  if (byteOffset >= buffer.length) return ''
  let start = byteOffset
  while (start < buffer.length && (buffer[start] & 0xc0) === 0x80) start += 1
  return buffer.subarray(start).toString('utf8')
}

function validateJobId(jobId) {
  if (typeof jobId !== 'string' || jobId === '' || byteLength(jobId) > 256) {
    throw new RpcError('jsonrpc/invalid-params', 'jobId must be a non-empty string')
  }
  return jobId
}

function mapRegistryError(operation, error) {
  const message = describe(error)
  if (message.startsWith('unknown job ') || message.includes('belongs to another session')) {
    return new RpcError('nano/jobs/not-found', `job not visible to this session: ${message}`)
  }
  return new RpcError('jsonrpc/internal-error', `${operation} failed: ${message}`)
}

function describe(error) {
  return error instanceof Error ? error.message : String(error)
}
