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
   *   authorizeChild?: (address: object) => Promise<object>,
   * }} options - host wiring.
   */
  constructor({
    ctx,
    writer,
    runtimeId,
    generation,
    rootSessionId,
    session,
    nonce,
    nonceRequired,
    authorizeChild,
  }) {
    this.ctx = ctx
    this.writer = writer
    this.runtimeId = runtimeId
    this.generation = generation
    this.rootSessionId = rootSessionId
    this.session = session
    this.nonce = nonce
    this.nonceRequired = nonceRequired
    this.authorizeChild = authorizeChild
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
    const target = await this.resolveAddress(address)
    validateKinds(kinds)

    const baseline = target.baseline
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
      address: target.address,
      sequence: 0,
      paused: true,
      buffer: [],
      closed: false,
      kinds: kinds === undefined ? ['session.event'] : kinds,
    }
    this.subscriptions.set(subscription.id, subscription)
    try {
      if (clamped < baseline && subscription.kinds.includes('session.event')) {
        if (target.childSessionId === undefined) {
          await this.replay(subscription, clamped, baseline, signal)
        } else {
          await this.replayChild(target.childSessionId, subscription, clamped, baseline, signal)
        }
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
      header: target.header,
    }
  }

  /**
   * Resolve one subscription address to its official baseline. Root
   * subscriptions use the live session; child subscriptions are authorized
   * against the official parent catalog and observed through the official
   * Session query without activating the child.
   * @param {object} address - wire address.
   * @returns {Promise<{address: object, baseline: number, header: object, childSessionId?: string}>} target.
   */
  async resolveAddress(address) {
    const normalized = validateAddress(address, this.rootSessionId)
    if (normalized.childSessionId === undefined) {
      return {
        address: normalized,
        baseline: this.session.seq,
        header: this.session.header,
      }
    }
    if (this.authorizeChild === undefined) {
      throw new RpcError('nano/subagents/unavailable', 'child subscriptions are not served by this generation')
    }
    const authorized = await this.authorizeChild(normalized)
    const query = this.ctx.get('sessionQuery')
    if (query?.observeSession === undefined) {
      throw new RpcError('nano/subagents/unavailable', 'the official session query service is not composed')
    }
    const observation = await query.observeSession(normalized.childSessionId)
    try {
      const cursor = Number.isInteger(observation.cursor) ? observation.cursor : -1
      return {
        address: authorized,
        baseline: cursor + 1,
        header: observation.header,
        childSessionId: normalized.childSessionId,
      }
    } finally {
      try {
        observation?.[Symbol.dispose]?.()
      } catch {
        // lease release is best-effort
      }
    }
  }

  /**
   * Replay one child's durable events from the official Session query. The
   * child log body is never read as a file; inherited fork seeds are ordinary
   * child events and remain addressed to this child.
   * @param {string} childSessionId - durable child id.
   * @param {object} subscription - pending subscription.
   * @param {number} start - first durable seq.
   * @param {number} baseline - first live seq.
   * @param {AbortSignal} signal - cancellation.
   */
  async replayChild(childSessionId, subscription, start, baseline, signal) {
    const query = this.ctx.get('sessionQuery')
    if (query?.observeSession === undefined) {
      throw new RpcError('nano/subagents/unavailable', 'the official session query service is not composed')
    }
    if (signal?.aborted) throw new RpcError('nano/cancelled', 'subscription was cancelled')
    const observation = await query.observeSession(childSessionId, { signal })
    try {
      const delivered = new Set()
      for (const event of observation.events ?? []) {
        if (event.seq < start || event.seq >= baseline) continue
        delivered.add(event.seq)
        this.deliver(subscription, sessionEventPayload(subscription.address, event))
        if (subscription.closed) break
      }
      subscription.delivered = delivered
    } finally {
      try {
        observation?.[Symbol.dispose]?.()
      } catch {
        // lease release is best-effort
      }
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
    for (const subscription of this.subscriptions.values()) {
      if (subscription.closed) continue
      const childSessionId = subscription.address.childSessionId
      if (childSessionId === undefined) {
        if (subject.id !== this.rootSessionId) continue
      } else if (subject.id !== childSessionId) {
        continue
      }
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
      // A child subscription only receives frames addressed to that exact
      // child. Root subscriptions still receive all frames and route by the
      // payload address in the client.
      if (
        subscription.address.childSessionId !== undefined &&
        payload?.address !== undefined &&
        payload.address.childSessionId !== subscription.address.childSessionId
      ) {
        continue
      }
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
  if (address.childSessionId === undefined) {
    if (address.parentSessionId !== undefined || address.mode !== undefined) {
      throw new RpcError('jsonrpc/invalid-params', 'a root address cannot carry child fields')
    }
    return { rootSessionId }
  }
  if (typeof address.parentSessionId !== 'string' || address.parentSessionId === '') {
    throw new RpcError('jsonrpc/invalid-params', 'address.parentSessionId is required for a child address')
  }
  if (
    address.mode !== 'continuable' &&
    address.mode !== 'one-shot' &&
    address.mode !== 'unknown'
  ) {
    throw new RpcError('jsonrpc/invalid-params', 'address.mode must be continuable, one-shot, or unknown')
  }
  return {
    rootSessionId,
    parentSessionId: address.parentSessionId,
    childSessionId: address.childSessionId,
    mode: address.mode,
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
