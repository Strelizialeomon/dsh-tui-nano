// Durable session-event subscription with a replay baseline.
//
// A subscription is addressed by the exact root session. The host serves only
// its own root session in S01; child-address subscriptions are reserved for
// S12 and are rejected rather than silently mapped to the root. Live events
// are fanned out with a per-subscription envelope sequence; a slow consumer is
// closed explicitly rather than being dropped silently.
import { randomBytes } from 'node:crypto'
import { EVENT_KINDS, LIMITS } from '../../protocol/index.mjs'
import { encodeFrame } from './frames.mjs'
import { RpcError } from './errors.mjs'

export class SubscriptionHub {
  /**
   * @param {{
   *   ctx: object,
   *   writer: import('./writer.mjs').BoundedWriter,
   *   runtimeId: string,
   *   generation: number,
   *   rootSessionId: string,
   *   session: object,
   *   nonce: string,
   *   nonceRequired: boolean,
   * }} options - host wiring.
   */
  constructor({ ctx, writer, runtimeId, generation, rootSessionId, session, nonce, nonceRequired }) {
    this.ctx = ctx
    this.writer = writer
    this.runtimeId = runtimeId
    this.generation = generation
    this.rootSessionId = rootSessionId
    this.session = session
    this.nonce = nonce
    this.nonceRequired = nonceRequired
    this.subscriptions = new Map()
    this.counter = 0
    this.closed = false
    this.stopListener = ctx.on('session/event', (subject, event) => this.handleSessionEvent(subject, event))
  }

  /**
   * Create one subscription and replay the requested durable prefix.
   * @param {{address: {rootSessionId: string, childSessionId?: string}, kinds?: string[], fromSeq?: number}} params - subscribe request.
   * @returns {Promise<{subscriptionId: string, address: object, seq: number, header: object}>} baseline response.
   */
  async subscribe(params, signal) {
    if (this.closed) throw new RpcError('nano/closed', 'host is shutting down')
    const { address, kinds, fromSeq } = params
    if (signal?.aborted) throw new RpcError('nano/cancelled', 'subscription was cancelled')
    validateAddress(address, this.rootSessionId)
    validateKinds(kinds)

    const baseline = this.session.seq
    const start = fromSeq === undefined ? baseline : fromSeq
    if (!Number.isInteger(start) || start < 0) {
      throw new RpcError('jsonrpc/invalid-params', 'fromSeq must be a non-negative integer')
    }
    // A client that tracks the next durable seq can legitimately send
    // baseline + 1. Clamp instead of failing the rebuild.
    const clamped = start > baseline ? baseline : start
    if (baseline - clamped > LIMITS.maxReplayEvents) {
      throw new RpcError(
        'jsonrpc/invalid-params',
        `replay window ${String(baseline - clamped)} exceeds maxReplayEvents ${String(LIMITS.maxReplayEvents)}; request a later fromSeq`,
      )
    }

    const subscription = {
      id: `sub-${String(++this.counter)}-${randomBytes(4).toString('hex')}`,
      address: { rootSessionId: this.rootSessionId },
      sequence: 0,
      paused: true,
      buffer: [],
      closed: false,
      kinds: kinds === undefined ? ['session.event'] : kinds,
    }
    this.subscriptions.set(subscription.id, subscription)
    try {
      if (clamped < baseline && subscription.kinds.includes('session.event')) {
        await this.replay(subscription, clamped, baseline, signal)
      }
      const delivered = subscription.delivered ?? new Set()
      for (const payload of subscription.buffer) {
        if (payload.seq >= clamped && !delivered.has(payload.seq)) this.deliver(subscription, payload)
      }
      subscription.buffer = []
      subscription.delivered = undefined
      subscription.paused = false
      if (subscription.closed) {
        // The writer closed the subscription because a replayed or live event
        // exceeded its per-event byte bound. Do not pretend the subscription
        // is usable; the client needs an explicit failure to stop retrying.
        throw new RpcError(
          'nano/unsupported',
          'subscription closed during replay because an event exceeded the per-event frame limit',
        )
      }
    } catch (error) {
      this.subscriptions.delete(subscription.id)
      this.writer.dropSubscription(subscription.id)
      if (error instanceof RpcError) throw error
      throw new RpcError('jsonrpc/internal-error', `subscription replay failed: ${error instanceof Error ? error.message : String(error)}`)
    }
    return {
      subscriptionId: subscription.id,
      address: subscription.address,
      seq: baseline,
      header: this.session.header,
    }
  }

  /**
   * Replay durable events from persistence so the client has an ordered
   * baseline. The live listener is already buffering; overlapping seqs are
   * deduplicated.
   * @param {object} subscription - pending subscription with `paused: true`.
   * @param {number} start - first durable seq to replay.
   * @param {number} baseline - live seq at subscribe time.
   */
  async replay(subscription, start, baseline, signal) {
    const persistence = this.ctx.get('sessionPersistence')
    if (persistence === undefined) {
      throw new RpcError('nano/unsupported', 'session persistence is not composed; replay is unavailable')
    }
    await this.ctx.sessions.flush(this.session)
    if (signal?.aborted) throw new RpcError('nano/cancelled', 'subscription was cancelled')
    const handle = await persistence.open(this.session.id, 'read')
    try {
      const result = await handle.read(start, Math.max(0, baseline - start), { signal })
      if (signal?.aborted) throw new RpcError('nano/cancelled', 'subscription was cancelled')
      const delivered = new Set()
      for (const event of result.events) {
        if (event.seq >= baseline) continue
        delivered.add(event.seq)
        this.deliver(subscription, sessionEventPayload(subscription.address, event))
        if (subscription.closed) break
      }
      subscription.delivered = delivered
    } finally {
      await handle.close()
    }
  }

  /**
   * Remove one subscription and tell the client why the stream ended.
   * @param {string} subscriptionId - subscription to cancel.
   * @returns {{}} empty result object.
   */
  unsubscribe(subscriptionId) {
    const subscription = this.subscriptions.get(subscriptionId)
    if (subscription === undefined) {
      throw new RpcError('nano/subscription-unknown', `unknown subscription ${subscriptionId}`)
    }
    this.closeSubscription(subscription, 'cancelled', 'client unsubscribed')
    return {}
  }

  /** Close every subscription during shutdown; never throws. */
  closeAll(reason = 'host shutting down', outcome = 'cancelled') {
    this.closed = true
    for (const subscription of [...this.subscriptions.values()]) {
      this.closeSubscription(subscription, outcome, reason)
    }
    try {
      this.stopListener?.()
    } catch {
      // listener disposal is best-effort during teardown
    }
  }

  handleSessionEvent(subject, event) {
    if (this.closed) return
    if (subject.id !== this.rootSessionId) return
    for (const subscription of this.subscriptions.values()) {
      if (subscription.closed) continue
      const payload = sessionEventPayload(subscription.address, event)
      if (subscription.paused) {
        subscription.buffer.push(payload)
      } else {
        this.deliver(subscription, payload)
      }
    }
  }

  /**
   * Fan out one live event of any contract kind. Durable session events have
   * their own listener and replay path; conversation events are live-only and
   * carry absolute projections, so a client that reconnects reads a snapshot.
   * @param {string} kind - contract event kind.
   * @param {object} payload - kind-specific payload.
   */
  publish(kind, payload) {
    if (this.closed) return
    if (!EVENT_KINDS.has(kind)) throw new Error(`unknown event kind ${kind}`)
    for (const subscription of this.subscriptions.values()) {
      if (subscription.closed || !subscription.kinds.includes(kind)) continue
      this.deliver(subscription, payload, kind)
    }
  }

  deliver(subscription, payload, kind = 'session.event') {
    if (subscription.closed) return
    if (!subscription.kinds.includes(kind)) return
    const frame = {
      jsonrpc: '2.0',
      method: 'nano.event',
      params: {
        runtimeId: this.runtimeId,
        generation: this.generation,
        subscriptionId: subscription.id,
        sequence: subscription.sequence,
        kind,
        payload,
      },
    }
    const buffer = encodeFrame(frame, { nonce: this.nonce, requireNonce: this.nonceRequired })
    const admitted = this.writer.enqueueEvent(subscription.id, buffer)
    if (admitted.ok) {
      subscription.sequence += 1
      return
    }
    this.closeSubscription(subscription, 'failed', admitted.reason)
  }

  closeSubscription(subscription, outcome, reason) {
    if (subscription.closed) return
    subscription.closed = true
    this.subscriptions.delete(subscription.id)
    this.writer.dropSubscription(subscription.id)
    const frame = {
      jsonrpc: '2.0',
      method: 'nano.streamEnded',
      params: {
        runtimeId: this.runtimeId,
        generation: this.generation,
        subscriptionId: subscription.id,
        outcome,
        reason,
      },
    }
    // Control notices bypass the event queue. A failed write is already being
    // handled by the writer's transport-error path.
    this.writer.sendControl(encodeFrame(frame, { nonce: this.nonce, requireNonce: this.nonceRequired })).catch(() => {})
  }
}

/**
 * Build one durable `session.event` payload. Envelope facts beyond seq/type/
 * data are forwarded only when the official event carries them: `time` is the
 * durable event time, `surfaceOp` marks a positional surface replacement (a
 * compaction summary shadows older history), and `sourceEventSeqs` cites
 * earlier sources.
 * @param {object} address - subscription address.
 * @param {object} event - official durable event.
 * @returns {object} contract payload.
 */
function sessionEventPayload(address, event) {
  const payload = {
    address,
    seq: event.seq,
    type: event.type,
    data: event.data,
  }
  if (Number.isInteger(event.time)) payload.time = event.time
  if (event.surfaceOp !== undefined) payload.surfaceOp = event.surfaceOp
  if (event.sourceEventSeqs !== undefined) payload.sourceEventSeqs = event.sourceEventSeqs
  return payload
}

function validateAddress(address, rootSessionId) {
  if (address === null || typeof address !== 'object' || Array.isArray(address)) {
    throw new RpcError('jsonrpc/invalid-params', 'address must be an object')
  }
  if (typeof address.rootSessionId !== 'string' || address.rootSessionId === '') {
    throw new RpcError('jsonrpc/invalid-params', 'address.rootSessionId must be a non-empty string')
  }
  if (address.rootSessionId !== rootSessionId) {
    throw new RpcError('nano/unsupported', 'address names a root session this host does not serve')
  }
  if (address.childSessionId !== undefined) {
    throw new RpcError('nano/unsupported', 'child-session subscriptions belong to S12 and are not served by this generation')
  }
}

function validateKinds(kinds) {
  if (kinds === undefined) return
  if (!Array.isArray(kinds) || kinds.some((kind) => typeof kind !== 'string')) {
    throw new RpcError('jsonrpc/invalid-params', 'kinds must be an array of strings')
  }
  for (const kind of kinds) {
    if (!EVENT_KINDS.has(kind)) {
      throw new RpcError('nano/unsupported', `event kind ${kind} is not served by this generation`)
    }
  }
}
