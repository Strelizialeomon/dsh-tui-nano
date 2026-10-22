// Subagent host: the official child catalog, lifecycle facts, child-session
// streaming, human continuation prompts, and the per-turn interrupt.
//
// S12 owns this module. Every catalog, mode, activity, timing and stop-reason
// fact comes from the official `ctx.subagents` service and the official
// Session query/projection services. The bridge never creates a child, never
// reads a child log file directly, and never invents a second catalog.
import { RpcError } from '../runtime/errors.mjs'

const MODES = new Set(['continuable', 'one-shot', 'external', 'unknown'])
const STOP_REASONS = new Set(['completed', 'aborted', 'error', 'max-tokens', 'refusal'])

/**
 * One subagent service per host generation, bound to the exact root Agent.
 */
export class SubagentsService {
  /**
   * @param {{
   *   ctx: object,
   *   hub: import('../runtime/events.mjs').SubscriptionHub,
   *   agent: object,
   *   session: object,
   *   rootSessionId: string,
   * }} options - host wiring.
   */
  constructor({ ctx, hub, agent, session, rootSessionId, attachments }) {
    this.ctx = ctx
    this.hub = hub
    this.agent = agent
    this.session = session
    this.rootSessionId = rootSessionId
    this.attachments = attachments
    this.closed = false
    this.disposers = []
    // Child ids seen in the catalog or lifecycle. Only these ids may stream
    // assistant frames into a child address.
    this.known = new Set()
    this.listen()
  }

  /** Official subagent service, or undefined when the profile omits it. */
  runtime() {
    return this.ctx.get('subagents')
  }

  /** The service is present and usable for listing. */
  available() {
    return this.runtime() !== undefined
  }

  /** Register lifecycle and child-stream listeners. */
  listen() {
    const on = (event, handler) => {
      const disposer = this.ctx.on(event, handler)
      if (typeof disposer === 'function') this.disposers.push(disposer)
    }
    on('subagent/start', (info) => {
      if (this.closed) return
      this.known.add(info.id)
      this.hub.publish('subagents.event', {
        type: 'start',
        childId: info.id,
        provider: info.provider,
        local: info.local === true,
        ...this.parentOf(info.id),
      })
    })
    on('subagent/end', (info) => {
      if (this.closed) return
      this.known.add(info.id)
      const stopReason = normalizeStopReason(info.stopReason)
      this.hub.publish('subagents.event', {
        type: 'end',
        childId: info.id,
        provider: info.provider,
        local: info.local === true,
        ...this.parentOf(info.id),
        ...(stopReason === undefined ? {} : { stopReason }),
      })
    })
    // Live assistant output for a known child uses the S02 stream event with
    // the child address; the S03 renderer consumes the same frame shape.
    on('agent/assistant-stream', (payload) => {
      if (this.closed) return
      const id = payload?.agent?.id
      if (typeof id !== 'string' || !this.known.has(id)) return
      const parentSessionId = this.parentOf(id).parentSessionId
      if (parentSessionId === undefined) return
      this.hub.publish('conversation.stream', {
        address: childAddress(this.rootSessionId, parentSessionId, id, 'continuable'),
        frame: payload.frame,
      })
    })
  }

  /** Parent identity for one child, when the durable child session exists. */
  parentOf(childId) {
    const child = this.ctx.agents?.get?.(childId)
    const parentSessionId = child?.session?.header?.parentSession
    return typeof parentSessionId === 'string' && parentSessionId !== ''
      ? { parentSessionId }
      : {}
  }

  /** Stop publishing; official children remain owned by the official service. */
  close() {
    this.closed = true
    for (const disposer of this.disposers.splice(0)) {
      try {
        disposer()
      } catch {
        // listener disposal is best-effort during teardown
      }
    }
  }

  /**
   * Direct children of the host root. Membership and mode come from the parent
   * catalog; activity is refined from the official live Agent registry; timing
   * comes from each child's own projection. No child Agent is activated.
   * @returns {Promise<{agents: object[], diagnostics: object[], parentLive: boolean}>} snapshot.
   */
  async snapshot() {
    const service = this.requireRuntime()
    const entries = await service.listChildren(this.rootSessionId)
    const agents = []
    const diagnostics = []
    for (const entry of entries) {
      this.known.add(entry.id)
      const row = this.rowFromEntry(entry, this.rootSessionId, 1)
      if (entry.mode !== 'external') {
        const timing = await this.readTiming(entry.id)
        if (timing.error !== undefined) {
          diagnostics.push({ id: entry.id, reason: timing.error })
        } else {
          Object.assign(row, timing.facts)
        }
      }
      agents.push(row)
    }
    return { agents, diagnostics, parentLive: this.parentLive(this.rootSessionId) }
  }

  /**
   * Recursive descendants in parent-catalog pre-order, including diagnostics for
   * unreadable branches. This is the official control-plane list: it never
   * activates a cold child and external rows stay leaves.
   * @returns {Promise<{entries: object[], diagnostics: object[]}>} list.
   */
  async list() {
    const service = this.requireRuntime()
    const entries = await service.listDescendants(this.rootSessionId)
    const rows = []
    const diagnostics = []
    for (const entry of entries) {
      if (entry.kind === 'diagnostic') {
        diagnostics.push({
          id: entry.id,
          reason: entry.reason,
          parentId: entry.parentId,
          depth: entry.depth,
        })
        continue
      }
      this.known.add(entry.id)
      rows.push(this.rowFromEntry(entry, entry.parentId, entry.depth, entry.hasChildren))
    }
    return { entries: rows, diagnostics }
  }

  /**
   * Resolve one child's official identity and timing. An `unknown` catalog mode
   * is confirmed from the child's own `subagent` projection; no Agent is
   * loaded or resumed.
   * @param {{parentSessionId: string, childSessionId: string}} params - address.
   * @returns {Promise<object>} description.
   */
  async describe(params) {
    const service = this.requireRuntime()
    const parentSessionId = requireId(params?.parentSessionId, 'parentSessionId')
    const childSessionId = requireId(params?.childSessionId, 'childSessionId')
    const children = await service.listChildren(parentSessionId)
    const catalog = children.find((entry) => entry.id === childSessionId)
    if (catalog === undefined) {
      throw new RpcError(
        'nano/subagents/unauthorized',
        `session ${childSessionId} is not a direct child of ${parentSessionId}`,
        { parentSessionId, childSessionId },
      )
    }
    this.known.add(childSessionId)
    const description = {
      id: childSessionId,
      parentId: parentSessionId,
      mode: catalog.mode,
      activity: this.activityOf(childSessionId),
      parentLive: this.parentLive(parentSessionId),
    }
    if (catalog.label !== undefined) description.label = catalog.label
    if (catalog.mode !== 'external') {
      const timing = await this.readTiming(childSessionId)
      if (timing.error !== undefined) {
        throw new RpcError('nano/subagents/unavailable', `child projection is unreadable: ${timing.error}`, {
          childSessionId,
        })
      }
      if (timing.identity?.mode !== undefined) description.mode = timing.identity.mode
      if (timing.identity?.label !== undefined) description.label = timing.identity.label
      if (Number.isInteger(timing.cursor)) description.seq = timing.cursor + 1
      Object.assign(description, timing.facts)
    }
    return description
  }

  /**
   * Send one human message to a direct continuable child through the official
   * `subagents/prompt` path. `sendMessage` is agent-to-agent only and is never
   * used here.
   * @param {object} params - address, requestId, delivery, content.
   * @param {AbortSignal} signal - cancellation before inbox acceptance.
   * @returns {Promise<{accepted: true, messageId: string}>} receipt.
   */
  async prompt(params, signal) {
    const service = this.requireRuntime()
    const address = this.validateAddress(params?.address, { continuableOnly: true })
    const requestId = requireId(params?.requestId, 'requestId')
    if (params?.delivery !== 'queue' && params?.delivery !== 'steer') {
      throw new RpcError('nano/subagents/invalid', 'delivery must be queue or steer')
    }
    if (this.attachments === undefined) {
      throw new RpcError('nano/subagents/unavailable', 'the attachment service is not composed')
    }
    const description = await this.describe({
      parentSessionId: address.parentSessionId,
      childSessionId: address.childSessionId,
    })
    if (description.mode !== 'continuable') {
      throw new RpcError(
        'nano/subagents/not-resumable',
        `child ${address.childSessionId} is not continuable (${description.mode})`,
        { childSessionId: address.childSessionId },
      )
    }
    if (!description.parentLive) {
      throw new RpcError(
        'nano/subagents/parent-unavailable',
        `parent ${address.parentSessionId} is not live; the child cannot accept a human message`,
        { parentSessionId: address.parentSessionId },
      )
    }
    // Resolve receipt-backed images only after the address, mode, and liveness
    // checks pass, so a rejected prompt cannot bind receipts it will not use.
    const resolved = this.attachments.resolveSubagentPrompt(address, params.content, requestId)
    try {
      const receipt = await service.prompt(
        {
          requestId,
          parentSessionId: address.parentSessionId,
          childSessionId: address.childSessionId,
          mode: 'continuable',
          delivery: params.delivery,
          content: resolved.parts,
        },
        signal,
      )
      this.attachments.commit(requestId, resolved.receipts)
      return { accepted: true, messageId: receipt.messageId }
    } catch (error) {
      this.attachments.releasePrompt(requestId, resolved.receipts)
      throw mapSubagentError(error, address)
    }
  }

  /**
   * Stop only the addressed child's current turn. Queued messages, the
   * activation and published descendants stay alive; `dispose` is not used.
   * @param {object} params - child address.
   * @returns {{accepted: true}} receipt.
   */
  interrupt(params) {
    const service = this.requireRuntime()
    const address = this.validateAddress(params?.address, { continuableOnly: true })
    try {
      service.interruptByParent(address.childSessionId, address.parentSessionId, 'continuable')
    } catch (error) {
      throw mapSubagentError(error, address)
    }
    return { accepted: true }
  }

  /**
   * Authorize one child subscription against the official catalog. The parent
   * must be the root or a reachable descendant, and a known mode must agree.
   * @param {object} address - wire child address.
   * @returns {Promise<object>} normalized address.
   */
  async authorizeChild(address) {
    const service = this.requireRuntime()
    const normalized = this.validateAddress(address, { continuableOnly: false })
    if (normalized.parentSessionId !== this.rootSessionId) {
      const reachable = await service.listDescendants(this.rootSessionId)
      const parent = reachable.find(
        (entry) => entry.kind === 'child' && entry.id === normalized.parentSessionId,
      )
      if (parent === undefined) {
        throw new RpcError(
          'nano/subagents/unauthorized',
          `parent ${normalized.parentSessionId} is not reachable from ${this.rootSessionId}`,
          { parentSessionId: normalized.parentSessionId },
        )
      }
    }
    const children = await service.listChildren(normalized.parentSessionId)
    const catalog = children.find((entry) => entry.id === normalized.childSessionId)
    if (catalog === undefined) {
      throw new RpcError(
        'nano/subagents/unauthorized',
        `session ${normalized.childSessionId} is not a direct child of ${normalized.parentSessionId}`,
        { parentSessionId: normalized.parentSessionId, childSessionId: normalized.childSessionId },
      )
    }
    if (
      normalized.mode !== 'unknown' &&
      catalog.mode !== 'unknown' &&
      normalized.mode !== catalog.mode
    ) {
      throw new RpcError(
        'nano/subagents/invalid',
        `address mode ${normalized.mode} disagrees with the catalog mode ${catalog.mode}`,
        { childSessionId: normalized.childSessionId },
      )
    }
    this.known.add(normalized.childSessionId)
    return normalized
  }

  /** Validate a wire child address and return its normalized spelling. */
  validateAddress(address, { continuableOnly }) {
    if (address === null || typeof address !== 'object' || Array.isArray(address)) {
      throw new RpcError('nano/subagents/invalid', 'address must be an object')
    }
    if (address.rootSessionId !== this.rootSessionId) {
      throw new RpcError('nano/subagents/invalid', 'address names a root session this host does not serve')
    }
    const parentSessionId = requireId(address.parentSessionId, 'address.parentSessionId')
    const childSessionId = requireId(address.childSessionId, 'address.childSessionId')
    if (typeof address.mode !== 'string' || !MODES.has(address.mode)) {
      throw new RpcError('nano/subagents/invalid', 'address.mode must be continuable, one-shot, external, or unknown')
    }
    if (continuableOnly && address.mode !== 'continuable') {
      throw new RpcError(
        'nano/subagents/not-resumable',
        `address mode ${address.mode} is not continuable`,
        { childSessionId },
      )
    }
    return {
      rootSessionId: this.rootSessionId,
      parentSessionId,
      childSessionId,
      mode: address.mode,
    }
  }

  requireRuntime() {
    const service = this.runtime()
    if (service === undefined) {
      throw new RpcError(
        'nano/subagents/unavailable',
        'the official subagent service is not composed in this profile',
      )
    }
    return service
  }

  /** One catalog or control-plane row in the wire shape. */
  rowFromEntry(entry, parentId, depth, hasChildren = false) {
    const row = {
      id: entry.id,
      parentId,
      depth,
      mode: entry.mode,
      activity: entry.activity ?? this.activityOf(entry.id),
      hasChildren: hasChildren === true,
    }
    if (entry.label !== undefined) row.label = entry.label
    return row
  }

  /** Official live-registry activity; absence is inactive, never guessed running. */
  activityOf(childId) {
    return this.ctx.agents?.get?.(childId)?.status === 'running' ? 'running' : 'inactive'
  }

  /** Whether the exact parent Agent is resident in the official registry. */
  parentLive(parentSessionId) {
    if (parentSessionId === this.rootSessionId) return true
    return this.ctx.agents?.get?.(parentSessionId) !== undefined
  }

  /**
   * Read one child's own `subagent` identity and `subagentTiming` projection.
   * Membership never depends on this read; a failure is reported as a
   * diagnostic by the caller.
   * @param {string} childId - durable child session id.
   * @returns {Promise<{facts: object, identity?: object, error?: string}>} timing facts or an error.
   */
  async readTiming(childId) {
    const query = this.ctx.get('sessionQuery')
    if (query?.observeSession === undefined) {
      return { facts: {}, error: 'sessionQuery is not composed' }
    }
    let observation
    try {
      observation = await query.observeSession(childId)
      const values = observation?.projections?.values ?? {}
      const timing = values.subagentTiming
      const identity = values.subagent
      const facts = {}
      if (Number.isFinite(timing?.settledMs)) facts.settledMs = timing.settledMs
      if (timing?.active !== undefined) {
        if (Number.isFinite(timing.active.since)) facts.activeSince = timing.active.since
        if (Number.isFinite(timing.active.through)) facts.activeThrough = timing.active.through
      }
      if (typeof timing?.lastTurnCompleted === 'boolean') {
        facts.lastTurnCompleted = timing.lastTurnCompleted
      }
      return {
        facts,
        cursor: Number.isInteger(observation?.cursor) ? observation.cursor : undefined,
        ...(identity === null || identity === undefined
          ? {}
          : {
              identity: {
                mode: identity.mode,
                ...(typeof identity.label === 'string' ? { label: identity.label } : {}),
              },
            }),
      }
    } catch (error) {
      return { facts: {}, error: describeError(error) }
    } finally {
      try {
        observation?.[Symbol.dispose]?.()
      } catch {
        // lease release is best-effort
      }
    }
  }
}

/** Build the S02 child address shape. */
function childAddress(rootSessionId, parentSessionId, childSessionId, mode) {
  return { rootSessionId, parentSessionId, childSessionId, mode }
}

/** Reject an empty or non-string identity. */
function requireId(value, field) {
  if (typeof value !== 'string' || value === '') {
    throw new RpcError('nano/subagents/invalid', `${field} must be a non-empty string`)
  }
  return value
}

/** Normalize an official stop reason; unknown values stay unproven. */
function normalizeStopReason(reason) {
  if (typeof reason !== 'string') return undefined
  return STOP_REASONS.has(reason) ? reason : 'unknown'
}

/** Map official control failures to the Nano error vocabulary. */
function mapSubagentError(error, address) {
  if (error instanceof RpcError) return error
  const code = typeof error?.code === 'string' ? error.code : ''
  const message = describeError(error)
  const data = { childSessionId: address.childSessionId, parentSessionId: address.parentSessionId }
  switch (code) {
    case 'subagent/parent-unavailable':
      return new RpcError('nano/subagents/parent-unavailable', message, data)
    case 'subagent/not-resumable':
      return new RpcError('nano/subagents/not-resumable', message, data)
    case 'subagent/unauthorized':
      return new RpcError('nano/subagents/unauthorized', message, data)
    case 'subagent/delivery-unavailable':
      return new RpcError('nano/subagents/delivery-unavailable', message, data)
    case 'subagent/attachment-invalid':
      return new RpcError('nano/subagents/attachment-invalid', message, data)
    case 'ACTIVATION_LIMIT_REACHED':
      return new RpcError('nano/subagents/capacity', message, data)
    case 'CONTINUATION_UNAVAILABLE':
      return new RpcError('nano/subagents/unavailable', message, data)
    default:
      return new RpcError('nano/subagents/invalid', message, data)
  }
}

/** Short, sanitized error text. */
function describeError(error) {
  if (error instanceof Error) return error.message
  return String(error)
}
