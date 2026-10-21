// Conversation host: prompt admission, the durable inbox projection, and live
// assistant-stream fan-out for the root session.
//
// S02 owns this module. It calls only the public Agent surface: `followup`,
// `steer`, `inbox` mutations and `cancel({ keepInbox: true })`. Message
// identity and dedup follow the official `user-rpc` source shape; the bridge
// never invents a second identity for the same requestId.
import { randomUUID } from 'node:crypto'
import { LIMITS } from '../../protocol/index.mjs'
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
   *   attachments?: import('../attachments/service.mjs').AttachmentService,
   * }} options - host wiring.
   */
  constructor({ ctx, hub, agent, session, rootSessionId, attachments }) {
    this.ctx = ctx
    this.hub = hub
    this.agent = agent
    this.session = session
    this.rootSessionId = rootSessionId
    this.attachments = attachments
    this.address = { rootSessionId }
    this.accepted = new Map()
    this.acceptedOrder = []
    this.inFlightRequests = new Map()
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
      on(event, (payload) => {
        if (event === 'agent/inbox/discarded') {
          this.forget(payload.message)
          this.attachments?.observeDiscarded(payload.message)
        }
        this.publishInbox()
      })
    }
    on('agent/assistant-stream', ({ frame }) => {
      this.hub.publish('conversation.stream', { address: this.address, frame })
    })
    const sessionDisposer = this.ctx.on('session/event', (subject, event) => {
      if (this.closed || subject.id !== this.rootSessionId || event.type !== 'user/message') return
      const rpcId = rpcIdOf(event.data)
      if (rpcId !== undefined) this.remember(rpcId, event.data.id)
      this.attachments?.observeUserMessage(event.data)
    })
    if (typeof sessionDisposer === 'function') this.disposers.push(sessionDisposer)
  }

  /** Stop publishing; the official agent stays alive for the shutdown path. */
  close() {
    this.closed = true
    this.attachments?.close()
    for (const disposer of this.disposers.splice(0)) {
      try {
        disposer()
      } catch {
        // listener disposal is best-effort during teardown
      }
    }
  }

  /**
   * Admit one ordered submission. `queue` is an ordinary follow-up turn;
   * `steer` is next-step input and is only offered while the Agent is running.
   * Attachment receipts are resolved and admitted before the Agent sees the
   * message, so a half-built attachment can never enter the session.
   * @param {object} params - `address`, `requestId`, `delivery`, `content`.
   * @returns {Promise<{accepted: true, duplicate: boolean, requestId: string, messageId: string}>} receipt.
   */
  async submit(params) {
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

    // A repeated requestId during admission must join the first attempt, not
    // start a second Agent message. This covers a client retry after an
    // unknown result while image admission is still in flight.
    const pending = this.inFlightRequests.get(params.requestId)
    if (pending !== undefined) return await pending
    const operation = this.admitAndSend(params, content)
    this.inFlightRequests.set(params.requestId, operation)
    try {
      return await operation
    } finally {
      this.inFlightRequests.delete(params.requestId)
    }
  }

  /**
   * Admit attachments and hand one message to the Agent. The caller owns the
   * per-request in-flight guard; this method assumes a fresh requestId.
   * @param {object} params - validated submit params.
   * @param {Array<object>} content - validated wire content.
   * @returns {Promise<object>} acceptance receipt.
   */
  async admitAndSend(params, content) {
    let admitted = content
    let receipts = []
    if (content.some((block) => block.type !== 'text')) {
      if (this.attachments === undefined) {
        throw new RpcError('nano/unsupported', 'attachments are not composed in this generation')
      }
      const result = await this.attachments.admitContent(params.address, content)
      admitted = result.content
      receipts = result.receipts
    }

    const messageId = randomUUID()
    const message = Object.freeze({
      id: messageId,
      role: 'user',
      content: Object.freeze(admitted.map((block) => Object.freeze(block))),
      source: Object.freeze({ kind: 'user', rpcId: params.requestId }),
    })
    // Bind the receipts before the Agent can synchronously observe the inbox
    // insertion; commit after acceptance, release on rejection.
    this.attachments?.bindPrompt(params.requestId, receipts)
    try {
      if (params.delivery === 'steer') this.agent.steer(message)
      else this.agent.followup(message)
    } catch (error) {
      this.attachments?.releasePrompt(params.requestId, receipts)
      throw new RpcError(
        'nano/conversation/rejected',
        `the Agent rejected the submission: ${error instanceof Error ? error.message : String(error)}`,
      )
    }
    this.attachments?.commit(params.requestId, receipts)
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
    const content = validateTextContent(params.content)
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
    const located = this.locateQueued(params.messageId)
    if (located === undefined) {
      throw new RpcError('nano/conversation/queue-item-not-found', 'queued item is no longer pending')
    }
    if (!this.agent.inbox.remove(params.messageId)) {
      throw new RpcError('nano/conversation/queue-item-not-found', 'queued item is no longer pending')
    }
    this.forget(located.message)
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

  /**
   * Publish the inbox projection, falling back to a bounded preview when the
   * full projection would exceed the per-event frame limit. A truncated
   * projection is display-only: the client must read a snapshot before editing.
   */
  publishInbox() {
    const snapshot = this.inboxSnapshot()
    this.attachments?.observeInbox(snapshot)
    this.hub.publish('conversation.inbox', this.inboxPayload())
  }

  /** Full projection when it fits; otherwise a byte-bounded preview. */
  inboxPayload() {
    const full = { address: this.address, inbox: this.inboxSnapshot() }
    const limit = LIMITS.maxEventFrameBytes - 2048
    if (Buffer.byteLength(JSON.stringify(full), 'utf8') <= limit) return full

    const budget = LIMITS.maxEventFrameBytes - 4096
    const total = full.inbox.nextTurn.length + full.inbox.nextStep.length
    const payload = {
      address: this.address,
      inbox: { nextTurn: [], nextStep: [] },
      truncated: true,
      total,
      omitted: 0,
    }
    let used = Buffer.byteLength(JSON.stringify(payload), 'utf8')
    const add = (items, target) => {
      for (const message of items) {
        const preview = {
          ...message,
          content: [{ type: 'text', text: codePointSlice(textOf(message.content), 1024) }],
        }
        const size = Buffer.byteLength(JSON.stringify(preview), 'utf8') + 1
        if (used + size > budget) {
          payload.omitted += 1
          continue
        }
        target.push(preview)
        used += size
      }
    }
    add(full.inbox.nextTurn, payload.inbox.nextTurn)
    add(full.inbox.nextStep, payload.inbox.nextStep)
    // The size estimate includes commas and brackets approximately; verify and
    // drop the tail until the serialized frame is safely inside the limit.
    while (
      Buffer.byteLength(JSON.stringify(payload), 'utf8') > limit &&
      (payload.inbox.nextStep.length > 0 || payload.inbox.nextTurn.length > 0)
    ) {
      if (payload.inbox.nextStep.length > 0) payload.inbox.nextStep.pop()
      else payload.inbox.nextTurn.pop()
      payload.omitted += 1
    }
    return payload
  }

  /** Drop the accepted cache entry for a message that was durably cancelled. */
  forget(message) {
    const rpcId = rpcIdOf(message)
    if (rpcId !== undefined) {
      this.accepted.delete(rpcId)
      this.acceptedOrder = this.acceptedOrder.filter((id) => id !== rpcId)
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
 * Validate one ordered content list. Text and attachment references keep their
 * user order; attachment receipts are resolved by the attachment host before
 * the Agent sees the message.
 * @param {unknown} content - wire content array.
 * @returns {Array<object>} normalized frozen blocks.
 */
export function validateContent(content) {
  if (!Array.isArray(content) || content.length === 0) {
    throw new RpcError('nano/conversation/empty', 'content must be a non-empty array')
  }
  const blocks = []
  let hasText = false
  let textBytes = 0
  for (const block of content) {
    if (block === null || typeof block !== 'object' || Array.isArray(block)) {
      throw new RpcError('nano/conversation/content', 'each content block must be an object')
    }
    if (block.type === 'text') {
      if (typeof block.text !== 'string') {
        throw new RpcError('nano/conversation/content', 'text blocks must carry a string')
      }
      if (block.text.trim() !== '') hasText = true
      textBytes += Buffer.byteLength(block.text, 'utf8')
      blocks.push(Object.freeze({ type: 'text', text: block.text }))
      continue
    }
    if (block.type === 'image' || block.type === 'file') {
      if (typeof block.receiptId !== 'string' || block.receiptId === '') {
        throw new RpcError('nano/conversation/content', `${block.type} blocks need a receiptId`)
      }
      blocks.push(Object.freeze({ type: block.type, receiptId: block.receiptId }))
      continue
    }
    throw new RpcError('nano/conversation/content', `content block type ${String(block.type)} is not supported`)
  }
  if (!hasText && !blocks.some((block) => block.type !== 'text')) {
    throw new RpcError('nano/conversation/empty', 'content needs non-whitespace text or an attachment')
  }
  if (textBytes > LIMITS.maxSubmitTextBytes) {
    throw new RpcError(
      'nano/conversation/too-large',
      `content is ${String(textBytes)} bytes, over the ${String(LIMITS.maxSubmitTextBytes)} byte submission limit`,
    )
  }
  return blocks
}

/**
 * Validate a queue edit: the official edit replaces the whole content with
 * text only, so attachment references are refused here.
 * @param {unknown} content - wire content array.
 * @returns {Array<{type: 'text', text: string}>} normalized frozen blocks.
 */
export function validateTextContent(content) {
  const blocks = validateContent(content)
  if (blocks.some((block) => block.type !== 'text')) {
    throw new RpcError('nano/conversation/content', 'queue edits accept only text content')
  }
  if (!blocks.some((block) => block.text.trim() !== '')) {
    throw new RpcError('nano/conversation/empty', 'content needs non-whitespace text')
  }
  return blocks
}

/** Slice by code point so a surrogate pair is never split. */
function codePointSlice(text, count) {
  return Array.from(text).slice(0, count).join('')
}

/** Concatenate text blocks for a bounded preview. */
function textOf(content) {
  if (!Array.isArray(content)) return ''
  return content
    .map((block) => (block?.type === 'text' && typeof block.text === 'string' ? block.text : `[${String(block?.type)}]`))
    .join('')
}

/** Read the official `user-rpc` correlation id from a message-shaped value. */
function rpcIdOf(value) {
  const source = value?.source
  if (source === undefined || source === null) return undefined
  if (source.kind !== 'user' || typeof source.rpcId !== 'string' || source.rpcId === '') return undefined
  return source.rpcId
}
