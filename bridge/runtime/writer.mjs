// Byte-bounded, priority-aware frame writer.
//
// One writer owns the output descriptor. Control frames (hello, responses,
// stream-ended notices) jump ahead of queued events; durable events are never
// silently dropped. If a subscription's queued bytes exceed the declared bound,
// the writer refuses the frame and the subscription owner closes that
// subscription with an explicit `nano.streamEnded`, which the client must
// recover from a baseline read.
import { LIMITS } from '../../protocol/index.mjs'

export class BoundedWriter {
  /**
   * @param {{
   *   write: (buffer: Buffer) => Promise<void>,
   *   onSlowSubscription?: (subscriptionId: string, reason: 'slow-consumer'|'event-too-large') => void,
   *   onWriteError?: (error: unknown) => void,
   *   maxEventFrameBytes?: number,
   *   maxSubscriptionQueueBytes?: number,
   *   maxTotalQueueBytes?: number,
   * }} options - output sink and bounds.
   */
  constructor({
    write,
    onSlowSubscription,
    onWriteError,
    maxEventFrameBytes = LIMITS.maxEventFrameBytes,
    maxSubscriptionQueueBytes = LIMITS.maxSubscriptionQueueBytes,
    maxTotalQueueBytes = LIMITS.maxTotalQueueBytes,
  }) {
    this.write = write
    this.onSlowSubscription = onSlowSubscription
    this.onWriteError = onWriteError
    this.maxEventFrameBytes = maxEventFrameBytes
    this.maxSubscriptionQueueBytes = maxSubscriptionQueueBytes
    this.maxTotalQueueBytes = maxTotalQueueBytes
    this.controlQueue = []
    this.eventQueue = []
    this.eventBytes = 0
    this.eventBytesBySubscription = new Map()
    this.draining = false
    this.inFlight = undefined
    this.failed = undefined
    this.closed = false
    this.flushWaiters = []
  }

  /** @returns {boolean} whether anything is queued or being written. */
  get busy() {
    return this.draining || this.controlQueue.length > 0 || this.eventQueue.length > 0
  }

  /**
   * Queue a control frame ahead of events. The returned promise settles when
   * this exact frame has been written (or the transport failed).
   * @param {Buffer} buffer - encoded frame.
   * @returns {Promise<void>} write completion.
   */
  sendControl(buffer) {
    if (this.failed !== undefined) return Promise.reject(this.failed)
    if (this.closed) return Promise.reject(new Error('writer is closed'))
    return new Promise((resolve, reject) => {
      this.controlQueue.push({ buffer, resolve, reject })
      this.kick()
    })
  }

  /**
   * Queue one event frame for a subscription.
   * @param {string} subscriptionId - owning subscription.
   * @param {Buffer} buffer - encoded event frame.
   * @returns {{ok: true} | {ok: false, reason: 'slow-consumer'|'event-too-large'|'transport-failed'}} admission result.
   */
  enqueueEvent(subscriptionId, buffer) {
    if (this.failed !== undefined) return { ok: false, reason: 'transport-failed' }
    if (this.closed) return { ok: false, reason: 'slow-consumer' }
    if (buffer.length > this.maxEventFrameBytes) return { ok: false, reason: 'event-too-large' }
    const current = this.eventBytesBySubscription.get(subscriptionId) ?? 0
    if (
      current + buffer.length > this.maxSubscriptionQueueBytes ||
      this.eventBytes + buffer.length > this.maxTotalQueueBytes
    ) {
      return { ok: false, reason: 'slow-consumer' }
    }
    this.eventBytesBySubscription.set(subscriptionId, current + buffer.length)
    this.eventBytes += buffer.length
    this.eventQueue.push({ buffer, subscriptionId })
    this.kick()
    return { ok: true }
  }

  /**
   * Remove every queued event belonging to one subscription.
   * @param {string} subscriptionId - subscription to drop.
   */
  dropSubscription(subscriptionId) {
    const kept = []
    for (const entry of this.eventQueue) {
      if (entry.subscriptionId === subscriptionId) {
        this.releaseEventBytes(subscriptionId, entry.buffer.length)
      } else {
        kept.push(entry)
      }
    }
    this.eventQueue = kept
    this.eventBytesBySubscription.delete(subscriptionId)
  }

  /**
   * Wait until every queued frame was written.
   * @param {{timeoutMs?: number}} [options] - upper bound for the wait.
   * @returns {Promise<void>} resolves when idle; rejects on timeout or transport failure.
   */
  flush({ timeoutMs = LIMITS.gracefulShutdownMs } = {}) {
    if (this.failed !== undefined) return Promise.reject(this.failed)
    if (!this.busy) return Promise.resolve()
    return new Promise((resolve, reject) => {
      const waiter = { resolve, reject }
      if (timeoutMs > 0) {
        waiter.timer = setTimeout(() => {
          this.flushWaiters = this.flushWaiters.filter((candidate) => candidate !== waiter)
          reject(new Error(`writer did not flush within ${String(timeoutMs)}ms`))
        }, timeoutMs)
        waiter.timer.unref?.()
      }
      this.flushWaiters.push(waiter)
    })
  }

  /** Stop accepting frames and reject anything still queued. */
  close() {
    this.closed = true
    const error = new Error('writer is closed')
    this.inFlight?.reject?.(error)
    this.inFlight = undefined
    for (const entry of this.controlQueue.splice(0)) entry.reject(error)
    this.eventQueue = []
    this.eventBytes = 0
    this.eventBytesBySubscription.clear()
    this.settleFlushWaiters(error)
  }

  kick() {
    if (this.draining || this.failed !== undefined) return
    this.draining = true
    void this.drain()
  }

  async drain() {
    try {
      while (true) {
        const entry = this.controlQueue.shift() ?? this.eventQueue.shift()
        if (entry === undefined) break
        this.inFlight = entry
        if (entry.subscriptionId !== undefined) this.releaseEventBytes(entry.subscriptionId, entry.buffer.length)
        try {
          await this.write(entry.buffer)
        } catch (error) {
          entry.reject?.(error)
          throw error
        }
        this.inFlight = undefined
        entry.resolve?.()
      }
    } catch (error) {
      this.failed = error
      this.inFlight?.reject?.(error)
      this.inFlight = undefined
      for (const entry of this.controlQueue.splice(0)) entry.reject(error)
      for (const entry of this.eventQueue.splice(0)) {
        if (entry.subscriptionId !== undefined) this.releaseEventBytes(entry.subscriptionId, entry.buffer.length)
      }
      this.settleFlushWaiters(error)
      this.onWriteError?.(error)
    } finally {
      this.draining = false
      if (!this.busy && this.failed === undefined) this.settleFlushWaiters()
    }
  }

  releaseEventBytes(subscriptionId, bytes) {
    const current = this.eventBytesBySubscription.get(subscriptionId) ?? 0
    const next = Math.max(0, current - bytes)
    if (next === 0) this.eventBytesBySubscription.delete(subscriptionId)
    else this.eventBytesBySubscription.set(subscriptionId, next)
    this.eventBytes = Math.max(0, this.eventBytes - bytes)
  }

  settleFlushWaiters(error) {
    const waiters = this.flushWaiters.splice(0)
    for (const waiter of waiters) {
      if (waiter.timer !== undefined) clearTimeout(waiter.timer)
      if (error === undefined) waiter.resolve()
      else waiter.reject(error)
    }
  }
}
