// Conversation host: prompt admission, the durable inbox projection, and live
// assistant-stream fan-out for the root session.
//
// S02 owns this module. It calls only the public Agent surface: `followup`,
// `steer`, `inbox` mutations and `cancel({ keepInbox: true })`. Message
// identity and dedup follow the official `user-rpc` source shape; the bridge
// never invents a second identity for the same requestId.
import { randomUUID } from 'node:crypto'
import { RpcError } from '../runtime/errors.mjs'

/**
 * One conversation service per host generation, bound to the root Agent.
 */
export class ConversationService {
  /**
   * @param {{
   *   ctx: object,
   *   hub: import('../runtime/events.mjs').SubscriptionHub,
   *   agent: object,
   *   session: object,
   *   rootSessionId: string,
   * }} options - host wiring.
   */
  constructor({ ctx, hub, agent, session, rootSessionId }) {
    this.ctx = ctx
    this.hub = hub
    this.agent = agent
    this.session = session
    this.rootSessionId = rootSessionId
    this.address = { rootSessionId }
    this.accepted = new Map()
    this.acceptedOrder = []
    this.disposers = []
    this.closed = false
    this.indexDurableRequests()
    this.listen()
  }

  /** Remember durable user messages so a repeated submit replays the original identity. */
  indexDurableRequests() {
    for (const event of this.session.snapshotEvents()) {
      if (event.type !== 'user/message') continue
      const rpcId = rpcIdOf(event.data)
      if (rpcId !== undefined) this.remember(rpcId, event.data.id)
    }
  }

  /** Register live listeners. Scoped Agent events reach the host context. */
  listen() {
    const on = (event, handler) => {
      const disposer = this.ctx.on(event, (payload) => {
        if (this.closed) return
        if (payload?.agent === undefined || payload.agent.id !== this.rootSessionId) return
        handler(payload)
      })
      if (typeof disposer === 'function') this.disposers.push(disposer)
    }
    on('agent/status', ({ status }) => {
      this.hub.publish('conversation.status', { address: this.address, status })
    })
    for (const event of ['agent/inbox/inserted', 'agent/inbox/claimed', 'agent/inbox/discarded']) {
      on(event, () => {
        this.hub.publish('conversation.inbox', { address: this.address, inbox: this.inboxSnapshot() })
      })
    }
    on('agent/assistant-stream', ({ frame }) => {
      this.hub.publish('conversation.stream', { address: this.address, frame })
    })
    const sessionDisposer = this.ctx.on('session/event', (subject, event) => {
      if (this.closed || subject.id !== this.rootSessionId || event.type !== 'user/message') return
      const rpcId = rpcIdOf(event.data)
      if (rpcId !== undefined) this.remember(rpcId, event.data.id)
    })
    if (typeof sessionDisposer === 'function') this.disposers.push(sessionDisposer)
  }

  /** Stop publishing; the official agent stays alive for the shutdown path. */
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
   * Admit one text submission. `queue` is an ordinary follow-up turn; `steer`
   * is next-step input and is only offered while the Agent is running.
   * @param {object} params - `address`, `requestId`, `delivery`, `content`.
   * @returns {{accepted: true, duplicate: boolean, requestId: string, messageId: string}} receipt.
   */
  submit(params) {
    this.assertAddress(params?.address)
    if (typeof params.requestId !== 'string' || params.requestId === '') {
      throw new RpcError('jsonrpc/invalid-params', 'requestId must be a non-empty string')
    }
    if (params.delivery !== 'queue' && params.delivery !== 'steer') {
      throw new RpcError('jsonrpc/invalid-params', 'delivery must be queue or steer')
    }
    const content = validateContent(params.content)

    const existing = this.findAccepted(params.requestId)
    if (existing !== undefined) {
      return { accepted: true, duplicate: true, requestId: params.requestId, messageId: existing }
    }
    if (params.delivery === 'steer' && this.agent.status !== 'running') {
      throw new RpcError('nano/conversation/steer-unavailable', 'the current turn no longer accepts steering')
    }

    const messageId = randomUUID()
    const message = Object.freeze({
      id: messageId,
      role: 'user',
      content: Object.freeze(content.map((block) => Object.freeze(block))),
      source: Object.freeze({ kind: 'user', rpcId: params.requestId }),
    })
    try {
      if (params.delivery === 'steer') this.agent.steer(message)
      else this.agent.followup(message)
    } catch (error) {
      throw new RpcError(
        'nano/conversation/rejected',
        `the Agent rejected the submission: ${error instanceof Error ? error.message : String(error)}`,
      )
    }
    this.remember(params.requestId, messageId)
    return { accepted: true, duplicate: false, requestId: params.requestId, messageId }
  }

  /**
   * Replace one pending text item. Items that carry any non-text block are
   * rejected: the official edit replaces the whole content and has no
   * remove-attachment action.
   * @param {object} params - `address`, `messageId`, `content`.
   * @returns {{accepted: true, messageId: string}} receipt.
   */
  editQueued(params) {
    this.assertAddress(params?.address)
    const content = validateContent(params.content)
    if (typeof params.messageId !== 'string' || params.messageId === '') {
      throw new RpcError('jsonrpc/invalid-params', 'messageId must be a non-empty string')
    }
    const located = this.locateQueued(params.messageId)
    if (located === undefined) {
      throw new RpcError('nano/conversation/queue-item-not-found', 'queued item is no longer pending')
    }
    if (located.message.content.some((block) => block.type !== 'text')) {
      throw new RpcError(
        'nano/conversation/queued-attachment',
        'queue edits cannot carry attachments; delete and resend instead',
      )
    }
    const replacement = Object.freeze({
      ...located.message,
      content: Object.freeze(content.map((block) => Object.freeze(block))),
    })
    const replaced = this.agent.inbox.replace(params.messageId, replacement)
    if (!replaced) {
      throw new RpcError('nano/conversation/queue-item-not-found', 'queued item is no longer pending')
    }
    return { accepted: true, messageId: params.messageId }
  }

  /**
   * Durably cancel one pending item. A claimed item is reported as missing so
   * the client rereads the queue instead of claiming an empty queue.
   * @param {object} params - `address`, `messageId`.
   * @returns {{removed: true}} receipt.
   */
  deleteQueued(params) {
    this.assertAddress(params?.address)
    if (typeof params.messageId !== 'string' || params.messageId === '') {
      throw new RpcError('jsonrpc/invalid-params', 'messageId must be a non-empty string')
    }
    if (this.locateQueued(params.messageId) === undefined) {
      throw new RpcError('nano/conversation/queue-item-not-found', 'queued item is no longer pending')
    }
    if (!this.agent.inbox.remove(params.messageId)) {
      throw new RpcError('nano/conversation/queue-item-not-found', 'queued item is no longer pending')
    }
    return { removed: true }
  }

  /**
   * Move one next-turn item to steering. The official rule is next-turn only
   * while running; a step-boundary item cannot be promoted.
   * @param {object} params - `address`, `messageId`.
   * @returns {{accepted: true}} receipt.
   */
  steerQueued(params) {
    this.assertAddress(params?.address)
    if (typeof params.messageId !== 'string' || params.messageId === '') {
      throw new RpcError('jsonrpc/invalid-params', 'messageId must be a non-empty string')
    }
    if (this.agent.status !== 'running') {
      throw new RpcError('nano/conversation/steer-unavailable', 'the current turn no longer accepts steering')
    }
    const message = this.agent.inbox.nextTurn.find((item) => item.id === params.messageId)
    if (message === undefined) {
      throw new RpcError('nano/conversation/steer-unavailable', 'only a queued next-turn item can be steered')
    }
    if (!this.agent.inbox.remove(params.messageId)) {
      throw new RpcError('nano/conversation/queue-item-not-found', 'queued item is no longer pending')
    }
    this.agent.steer(message)
    return { accepted: true }
  }

  /**
   * Stop the active turn but keep queued work. The receipt means the request
   * was admitted, not that the model already stopped.
   * @param {object} params - `address`.
   * @returns {{accepted: true}} receipt.
   */
  cancel(params) {
    this.assertAddress(params?.address)
    this.agent.cancel({ kind: 'user' }, { keepInbox: true })
    return { accepted: true }
  }

  /**
   * Read the current official baseline after a reconnect. Absolute values let
   * the client apply later absolute events without replaying deltas.
   * @param {object} params - `address`.
   * @returns {{address: object, seq: number, status: string, inbox: object}} snapshot.
   */
  snapshot(params) {
    this.assertAddress(params?.address)
    return {
      address: this.address,
      seq: this.session.seq,
      status: this.agent.status,
      inbox: this.inboxSnapshot(),
    }
  }

  /** Full pending-input projection in contract order. */
  inboxSnapshot() {
    return {
      nextTurn: [...this.agent.inbox.nextTurn],
      nextStep: [...this.agent.inbox.nextStep],
    }
  }

  /** Find a queued message across both pending lists. */
  locateQueued(messageId) {
    for (const target of ['nextTurn', 'nextStep']) {
      const message = this.agent.inbox[target].find((item) => item.id === messageId)
      if (message !== undefined) return { target, message }
    }
    return undefined
  }

  /** Find a message already accepted for this requestId, pending or durable. */
  findAccepted(requestId) {
    const indexed = this.accepted.get(requestId)
    if (indexed !== undefined) return indexed
    for (const target of ['nextTurn', 'nextStep']) {
      const message = this.agent.inbox[target].find((item) => rpcIdOf(item) === requestId)
      if (message !== undefined) {
        this.remember(requestId, message.id)
        return message.id
      }
    }
    return undefined
  }

  /** Remember one accepted request without growing without bound. */
  remember(requestId, messageId) {
    if (!this.accepted.has(requestId)) this.acceptedOrder.push(requestId)
    this.accepted.set(requestId, messageId)
    while (this.acceptedOrder.length > 1024) {
      const oldest = this.acceptedOrder.shift()
      this.accepted.delete(oldest)
    }
  }

  /** Only the exact root address is served in S02; children belong to S12. */
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
}

/**
 * Validate an ordered text-only content list. Attachments are S06's; accepting
 * an unknown block here would submit a half-built attachment to the official
 * session, so it is rejected instead.
 * @param {unknown} content - wire content array.
 * @returns {Array<{type: 'text', text: string}>} normalized frozen blocks.
 */
export function validateContent(content) {
  if (!Array.isArray(content) || content.length === 0) {
    throw new RpcError('nano/conversation/empty', 'content must be a non-empty array')
  }
  const blocks = []
  for (const block of content) {
    if (block === null || typeof block !== 'object' || Array.isArray(block)) {
      throw new RpcError('nano/conversation/content', 'each content block must be an object')
    }
    if (block.type !== 'text' || typeof block.text !== 'string') {
      throw new RpcError('nano/conversation/content', `content block type ${String(block.type)} is not supported by S02`)
    }
    blocks.push(Object.freeze({ type: 'text', text: block.text }))
  }
  if (!blocks.some((block) => block.text.trim() !== '')) {
    throw new RpcError('nano/conversation/empty', 'content needs non-whitespace text')
  }
  return blocks
}

/** Read the official `user-rpc` correlation id from a message-shaped value. */
function rpcIdOf(value) {
  const source = value?.source
  if (source === undefined || source === null) return undefined
  if (source.kind !== 'user' || typeof source.rpcId !== 'string' || source.rpcId === '') return undefined
  return source.rpcId
}
