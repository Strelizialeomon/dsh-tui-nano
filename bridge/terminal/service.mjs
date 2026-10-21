// S09 user-terminal host: the only bridge surface that creates, retains,
// attaches, writes to and closes the official interactive terminal service.
//
// The official controller is built for Remote (Gateway) callers; its `attach`
// method reads `ctx.invocation.uplink()`. In-process Nano calls synthesize that
// one invocation (an async input queue) instead of mounting a Gateway Peer.
// Everything else is the public `ctx.terminalController` surface: create,
// retain, attach, rename, close, list, environment and shells.
import { randomBytes } from 'node:crypto'
import { ERROR_CODES } from '../../protocol/index.mjs'
import { encodeFrame } from '../runtime/frames.mjs'
import { RpcError } from '../runtime/errors.mjs'

const TERMINAL_ID = /^[\w-]{1,128}$/u
const MAX_TITLE_CHARS = 120

const REMOTE_ERRORS = new Map([
  ['terminal/unavailable', 'nano/terminal/unavailable'],
  ['terminal/limit-reached', 'nano/terminal/limit-reached'],
  ['terminal/control-unavailable', 'nano/terminal/control-unavailable'],
])

/** Bounded, single-consumer async input queue for one attachment. */
export class AttachmentInput {
  constructor() {
    this.items = []
    this.waiters = []
    this.error = undefined
    this.closed = false
  }

  /** @param {object} item - official TerminalInput frame. */
  push(item) {
    if (this.closed) return
    const waiter = this.waiters.shift()
    if (waiter !== undefined) waiter.resolve({ value: item, done: false })
    else this.items.push(item)
  }

  /** Fail the attachment without stopping the terminal process. */
  fail(error) {
    if (this.closed) return
    this.closed = true
    this.error = error
    for (const waiter of this.waiters.splice(0)) waiter.reject(error)
  }

  /** Half-close input. */
  close() {
    if (this.closed) return
    this.closed = true
    for (const waiter of this.waiters.splice(0)) waiter.resolve({ value: undefined, done: true })
  }

  /**
   * Official uplink iterable. `return()` must unblock a pending `next()`:
   * the provider's attach generator awaits it during output drainage, so a
   * generator suspended on a never-resolving input promise would deadlock the
   * final `done` and strand the client attachment.
   * @returns {AsyncIterableIterator<object>} single-consumer iterator.
   */
  [Symbol.asyncIterator]() {
    return {
      next: () => this.next(),
      return: async () => {
        this.close()
        return { value: undefined, done: true }
      },
      [Symbol.asyncIterator]() {
        return this
      },
    }
  }

  /** @returns {Promise<{value?: object, done: boolean}>} next command. */
  async next() {
    while (true) {
      if (this.items.length > 0) {
        return { value: this.items.shift(), done: false }
      }
      if (this.error !== undefined) {
        const error = this.error
        this.error = undefined
        throw error
      }
      if (this.closed) return { value: undefined, done: true }
      const next = await new Promise((resolve, reject) => this.waiters.push({ resolve, reject }))
      if (next.done) return { value: undefined, done: true }
      return { value: next.value, done: false }
    }
  }
}

/**
 * One terminal service per host generation, scoped to the root Agent/Session.
 * Output and input are live notifications; nothing here is persisted as a
 * durable session event.
 */
export class TerminalService {
  /**
   * @param {{
   *   ctx: object,
   *   writer: import('../runtime/writer.mjs').BoundedWriter,
   *   agent: object,
   *   session: object,
   *   rootSessionId: string,
   *   nonce: string,
   *   nonceRequired: boolean,
   * }} options - host wiring.
   */
  constructor({ ctx, writer, agent, session, rootSessionId, nonce, nonceRequired }) {
    this.ctx = ctx
    this.writer = writer
    this.agent = agent
    this.session = session
    this.rootSessionId = rootSessionId
    this.nonce = nonce
    this.nonceRequired = nonceRequired
    this.controller = ctx.get('terminalController')
    this.terminals = new Map()
    this.attachments = new Map()
    this.holds = new Map()
    this.counter = 0
    this.closed = false
  }

  /** @returns {boolean} whether the official controller is composed. */
  get available() {
    return this.controller !== undefined
  }

  /** Read the Session environment and limits. */
  environment(signal) {
    this.assertAvailable()
    try {
      return this.controller.environment(this.agent, signal ?? new AbortController().signal)
    } catch (error) {
      throw mapRemoteError(error, 'read terminal environment')
    }
  }

  /** Discover verified shells. */
  async shells(signal) {
    this.assertAvailable()
    try {
      return await this.controller.shells(this.agent, signal ?? new AbortController().signal)
    } catch (error) {
      throw mapRemoteError(error, 'discover shells')
    }
  }

  /**
   * List terminals retained by this Host generation.
   * @returns {{terminals: object[]}} sanitized terminal metadata.
   */
  list() {
    this.assertAvailable()
    return { terminals: this.controller.list(this.session.id).map(sanitizeInfo) }
  }

  /**
   * Allocate one terminal for a pre-generated identity. The official create is
   * idempotent while the identity is open.
   * @param {{terminalId: string, cols: number, rows: number, shellPath?: string}} params - create request.
   * @param {AbortSignal} [signal] - request cancellation.
   * @returns {Promise<{terminal: object}>} terminal metadata.
   */
  async create(params, signal) {
    this.assertAvailable()
    this.assertLive()
    const id = requireId(params?.terminalId)
    requireDimensions(params?.cols, params?.rows)
    const request = { id, cols: params.cols, rows: params.rows }
    if (params.shellPath !== undefined) {
      if (typeof params.shellPath !== 'string' || params.shellPath === '') {
        throw new RpcError('jsonrpc/invalid-params', 'shellPath must be a non-empty string')
      }
      request.shellPath = params.shellPath
    }
    try {
      const info = await this.controller.create(
        this.agent,
        request,
        signal ?? new AbortController().signal,
      )
      this.terminals.set(id, { id, info: sanitizeInfo(info), holds: new Set(), attachment: undefined })
      return { terminal: sanitizeInfo(info) }
    } catch (error) {
      throw mapRemoteError(error, 'create terminal')
    }
  }

  /**
   * Hold one terminal for this window, independently of output attachment.
   * @param {{terminalId: string}} params - retain request.
   * @param {AbortSignal} [signal] - request cancellation.
   * @returns {Promise<{holdId: string}>} hold identity.
   */
  async retain(params, signal) {
    this.assertAvailable()
    this.assertLive()
    const id = requireId(params?.terminalId)
    const holdId = `hold-${String(++this.counter)}-${randomBytes(4).toString('hex')}`
    const controller = new AbortController()
    const record = { id: holdId, terminalId: id, controller, released: false, iterator: undefined }
    try {
      const stream = this.controller.retain(this.session.id, id, controller.signal)
      const iterator = stream[Symbol.asyncIterator]()
      record.iterator = iterator
      const first = await iterator.next()
      if (first.done || first.value?.type !== 'retained') {
        throw new Error('retain stream ended before its acknowledgement')
      }
      if (signal?.aborted) {
        controller.abort(new Error('retain request was cancelled'))
        throw new Error('retain request was cancelled')
      }
      this.holds.set(holdId, record)
      this.terminals.get(id)?.holds.add(holdId)
      // The stream stays open until release/close; keep consuming it so the
      // official lifetime is actually held.
      void consumeHold(record, this)
      return { holdId }
    } catch (error) {
      controller.abort(error)
      throw mapRemoteError(error, 'retain terminal')
    }
  }

  /** Release one window hold; the process becomes eligible for unattended cleanup. */
  release(params) {
    this.assertAvailable()
    const holdId = params?.holdId
    if (typeof holdId !== 'string' || holdId === '') {
      throw new RpcError('jsonrpc/invalid-params', 'holdId must be a non-empty string')
    }
    const record = this.holds.get(holdId)
    if (record === undefined) return { released: false }
    this.holds.delete(holdId)
    this.terminals.get(record.terminalId)?.holds.delete(holdId)
    record.released = true
    record.controller.abort(new Error('window released the terminal hold'))
    if (typeof record.iterator?.return === 'function') {
      Promise.resolve(record.iterator.return()).catch(() => {})
    }
    return { released: true }
  }

  /**
   * Attach the current window to one terminal. The first official frame is a
   * bounded screen snapshot and is returned in the response; later output,
   * state and acknowledgements are live notifications.
   * @param {{terminalId: string}} params - attach request.
   * @param {AbortSignal} [signal] - request cancellation before the snapshot.
   * @returns {Promise<object>} attachment baseline.
   */
  async attach(params, signal) {
    this.assertAvailable()
    this.assertLive()
    const id = requireId(params?.terminalId)
    if (!this.terminals.has(id)) {
      // A recovered identity from a lost create response may already exist in
      // the official owner; only an explicit list/create may register it.
      const listed = this.controller.list(this.session.id).find((info) => info.id === id)
      if (listed === undefined) throw new RpcError('nano/terminal/not-found', 'terminal is not open in this host')
      this.terminals.set(id, { id, info: sanitizeInfo(listed), holds: new Set(), attachment: undefined })
    }
    const record = this.terminals.get(id)
    let environment
    try {
      environment = this.controller.environment(this.agent, new AbortController().signal)
    } catch (error) {
      throw mapRemoteError(error, 'read terminal environment')
    }
    if (record.attachment !== undefined) this.detachAttachment(record.attachment, 'detached')
    const attachmentId = `att-${String(++this.counter)}-${randomBytes(4).toString('hex')}`
    const input = new AttachmentInput()
    const controller = new AbortController()
    const attachment = {
      id: attachmentId,
      terminalId: id,
      input,
      controller,
      iterator: undefined,
      forward: undefined,
      detaching: false,
      closeRequested: false,
      endedSent: false,
      info: record.info,
      lastInputSequence: 0,
      maxInputBytes: environment.maxInputBytes,
      maxBufferedBytes: environment.maxBufferedBytes,
      maxCols: environment.maxCols,
      maxRows: environment.maxRows,
    }
    if (signal?.aborted) throw new RpcError('nano/cancelled', 'attach request was cancelled')
    try {
      const stream = this.attachStream(id, input, controller.signal)
      const iterator = stream[Symbol.asyncIterator]()
      attachment.iterator = iterator
      const first = await iterator.next()
      if (first.done || first.value?.type !== 'snapshot') {
        throw new Error('terminal attachment ended before its screen snapshot')
      }
      const frame = first.value
      attachment.info = sanitizeInfo(frame.info)
      record.info = attachment.info
      record.attachment = attachment
      this.attachments.set(attachmentId, attachment)
      attachment.forward = this.forward(attachment)
      return {
        attachmentId,
        terminalId: id,
        sequence: integer(frame.sequence, 'snapshot sequence'),
        writable: frame.writable === true && attachment.info.state === 'running',
        info: attachment.info,
        screen: String(frame.screen ?? ''),
      }
    } catch (error) {
      controller.abort(error)
      input.close()
      throw mapRemoteError(error, 'attach terminal')
    }
  }

  /** Stop an attachment without stopping its process. */
  detach(params) {
    this.assertAvailable()
    const attachmentId = params?.attachmentId
    if (typeof attachmentId !== 'string' || attachmentId === '') {
      throw new RpcError('jsonrpc/invalid-params', 'attachmentId must be a non-empty string')
    }
    const attachment = this.attachments.get(attachmentId)
    if (attachment === undefined) return { detached: false }
    this.detachAttachment(attachment, 'detached')
    return { detached: true }
  }

  /** Rename one terminal. */
  rename(params) {
    this.assertAvailable()
    const id = requireId(params?.terminalId)
    const title = params?.title
    if (typeof title !== 'string' || title.trim() === '' || title.length > MAX_TITLE_CHARS) {
      throw new RpcError('jsonrpc/invalid-params', 'title must contain 1-120 characters')
    }
    try {
      this.controller.rename(this.agent, id, title.trim())
      const record = this.terminals.get(id)
      if (record !== undefined) record.info = { ...record.info, title: title.trim() }
      return { renamed: true }
    } catch (error) {
      throw mapRemoteError(error, 'rename terminal')
    }
  }

  /**
   * Close one identity to future creation and terminate its process range.
   * Waits for official cleanup and final output; a failure keeps the terminal
   * retryable and is reported as an error.
   */
  async close(params) {
    this.assertAvailable()
    this.assertLive()
    const id = requireId(params?.terminalId)
    const record = this.terminals.get(id)
    const attachment = record?.attachment
    if (attachment !== undefined) {
      attachment.closeRequested = true
      attachment.input.close()
    }
    try {
      await this.controller.close(this.agent, id)
    } catch (error) {
      if (attachment !== undefined) attachment.closeRequested = false
      throw mapRemoteError(error, 'close terminal')
    }
    if (attachment !== undefined) {
      // The official close resolves after final output drainage; give the
      // forwarder a bounded chance to deliver the final state, then stop it.
      await Promise.race([attachment.forward, delay(1000)])
      this.detachAttachment(attachment, 'closed')
    }
    for (const holdId of [...(record?.holds ?? [])]) this.release({ holdId })
    this.terminals.delete(id)
    return { closed: true }
  }

  /** Stop every attachment and process owned by this host generation. */
  async closeAll() {
    for (const attachment of [...this.attachments.values()]) {
      this.detachAttachment(attachment, 'closed')
    }
    for (const holdId of [...this.holds.keys()]) this.release({ holdId })
    if (this.controller === undefined) {
      this.closed = true
      return
    }
    const failures = []
    for (const id of [...this.terminals.keys()]) {
      try {
        await this.controller.close(this.agent, id)
      } catch (error) {
        failures.push(`${id}: ${describe(error)}`)
      }
      this.terminals.delete(id)
    }
    this.closed = true
    if (failures.length > 0) throw new Error(`terminal cleanup failed (${failures.join('; ')})`)
  }

  /**
   * Route one client-to-host terminal notification.
   * @param {string} method - notification name.
   * @param {object} params - notification params.
   * @returns {boolean} whether the method belongs to S09.
   */
  handleNotification(method, params) {
    if (!method.startsWith('nano.terminal.')) return false
    if (method === 'nano.terminal.input') {
      this.writeInput(params)
      return true
    }
    if (method === 'nano.terminal.resize') {
      this.resize(params)
      return true
    }
    return true
  }

  /** Validate one ordered input frame before yielding it to the provider. */
  writeInput(params) {
    const attachment = this.attachment(params?.attachmentId)
    if (attachment === undefined) return
    const sequence = attachment.lastInputSequence + 1
    if (!Number.isSafeInteger(params.sequence) || params.sequence !== sequence) {
      this.failAttachment(attachment, 'input-failed', `input sequence gap: expected ${String(sequence)}`)
      return
    }
    if (typeof params.bytes !== 'string') {
      this.failAttachment(attachment, 'input-failed', 'input bytes must be a UTF-8 string')
      return
    }
    const bytes = Buffer.from(params.bytes, 'utf8')
    if (bytes.length > attachment.maxInputBytes) {
      this.failAttachment(
        attachment,
        'input-failed',
        `inputFull: command exceeds maxInputBytes ${String(attachment.maxInputBytes)}`,
      )
      return
    }
    attachment.lastInputSequence = sequence
    attachment.input.push({ type: 'write', sequence, data: params.bytes })
  }

  /** Validate one ordered resize command. */
  resize(params) {
    const attachment = this.attachment(params?.attachmentId)
    if (attachment === undefined) return
    const sequence = attachment.lastInputSequence + 1
    if (!Number.isSafeInteger(params.sequence) || params.sequence !== sequence) {
      this.failAttachment(attachment, 'input-failed', `resize sequence gap: expected ${String(sequence)}`)
      return
    }
    if (
      !Number.isSafeInteger(params.cols) ||
      !Number.isSafeInteger(params.rows) ||
      params.cols < 2 ||
      params.rows < 1 ||
      params.cols > attachment.maxCols ||
      params.rows > attachment.maxRows
    ) {
      this.failAttachment(
        attachment,
        'input-failed',
        `resize is outside 2..${String(attachment.maxCols)} columns or 1..${String(attachment.maxRows)} rows`,
      )
      return
    }
    attachment.lastInputSequence = sequence
    attachment.input.push({ type: 'resize', sequence, cols: params.cols, rows: params.rows })
  }

  /** Build an in-process invocation for the official attach uplink. */
  attachStream(id, input, signal) {
    const invocation = Object.freeze({ uplink: () => input, uplinkWindow: undefined })
    const callCtx = this.controller.ctx.extend({ invocation })
    const controller = this.controller
    const proxy = new Proxy(controller, {
      get(target, property) {
        if (property === 'ctx') return callCtx
        return Reflect.get(target, property, target)
      },
    })
    return proxy.attach(this.agent, id, signal)
  }

  /** Forward official frames as live Nano notifications. */
  async forward(attachment) {
    let endedReason = 'detached'
    let endedDetail
    try {
      while (true) {
        const next = await attachment.iterator.next()
        if (next.done) break
        const frame = next.value
        if (frame.type === 'output') {
          await this.emit('nano.terminal.output', {
            attachmentId: attachment.id,
            terminalId: attachment.terminalId,
            sequence: integer(frame.sequence, 'output sequence'),
            data: String(frame.data ?? ''),
          })
        } else if (frame.type === 'state') {
          attachment.info = sanitizeInfo(frame.info)
          const record = this.terminals.get(attachment.terminalId)
          if (record !== undefined) record.info = attachment.info
          await this.emit('nano.terminal.state', {
            attachmentId: attachment.id,
            terminalId: attachment.terminalId,
            writable: frame.writable === true && attachment.info.state === 'running',
            info: attachment.info,
          })
        } else if (frame.type === 'ack') {
          await this.emit('nano.terminal.ack', {
            attachmentId: attachment.id,
            sequence: integer(frame.sequence, 'ack sequence'),
            ...(frame.refusal === undefined ? {} : { refusal: frame.refusal }),
          })
        }
        // A second snapshot would mean the provider restarted the baseline;
        // the contract promises one per attachment.
      }
      endedReason = attachment.closeRequested
        ? 'closed'
        : attachment.info?.state === 'exited'
          ? 'drained'
          : attachment.detaching
            ? 'detached'
            : 'failed'
      if (endedReason === 'failed') endedDetail = 'terminal attachment ended without a final process state'
    } catch (error) {
      if (attachment.detaching || attachment.closeRequested || attachment.controller.signal.aborted) {
        endedReason = attachment.closeRequested ? 'closed' : 'detached'
      } else {
        endedReason = 'failed'
        endedDetail = describe(error)
      }
    } finally {
      this.attachments.delete(attachment.id)
      const record = this.terminals.get(attachment.terminalId)
      if (record?.attachment === attachment) record.attachment = undefined
      this.sendEnded(attachment, endedReason, endedDetail).catch(() => {})
    }
  }

  /** Send one terminal notification; output awaits the writer so a slow fd
   * applies backpressure instead of growing an unbounded queue. */
  async emit(method, params) {
    if (this.writer === undefined) return
    const frame = { jsonrpc: '2.0', method, params }
    await this.writer.sendControl(
      encodeFrame(frame, { nonce: this.nonce, requireNonce: this.nonceRequired }),
    )
  }

  /** Send at most one attachment-ended notification per attachment. */
  async sendEnded(attachment, reason, detail) {
    if (attachment.endedSent) return
    attachment.endedSent = true
    await this.emit('nano.terminal.ended', {
      attachmentId: attachment.id,
      terminalId: attachment.terminalId,
      reason,
      ...(detail === undefined ? {} : { detail }),
    })
  }

  attachment(attachmentId) {
    if (typeof attachmentId !== 'string' || attachmentId === '') return undefined
    return this.attachments.get(attachmentId)
  }

  failAttachment(attachment, reason, detail) {
    this.attachments.delete(attachment.id)
    const record = this.terminals.get(attachment.terminalId)
    if (record?.attachment === attachment) record.attachment = undefined
    attachment.input.fail(new Error(detail))
    attachment.controller.abort(new Error(detail))
    this.sendEnded(attachment, reason, detail).catch(() => {})
  }

  detachAttachment(attachment, reason) {
    if (this.attachments.get(attachment.id) !== attachment) return
    attachment.detaching = true
    attachment.input.close()
    attachment.controller.abort(new Error(`attachment ${reason}`))
    this.attachments.delete(attachment.id)
    const record = this.terminals.get(attachment.terminalId)
    if (record?.attachment === attachment) record.attachment = undefined
    if (typeof attachment.iterator?.return === 'function') {
      Promise.resolve(attachment.iterator.return()).catch(() => {})
    }
    this.sendEnded(attachment, reason).catch(() => {})
  }

  assertAvailable() {
    if (this.controller === undefined) {
      throw new RpcError(
        'nano/terminal/unavailable',
        'the official terminal controller is not composed in this profile',
      )
    }
  }

  assertLive() {
    if (this.closed) throw new RpcError('nano/closed', 'terminal service is closing')
  }
}

/** Consume the remaining retain frames until release/disposal. */
async function consumeHold(record, service) {
  try {
    while (true) {
      const next = await record.iterator.next()
      if (next.done) break
    }
  } catch {
    // cancellation is the normal release path
  } finally {
    if (service.holds.get(record.id) === record) service.holds.delete(record.id)
    service.terminals.get(record.terminalId)?.holds.delete(record.id)
    record.controller.abort(new Error('retain stream ended'))
  }
}

function requireId(value) {
  if (typeof value !== 'string' || !TERMINAL_ID.test(value)) {
    throw new RpcError('jsonrpc/invalid-params', 'terminalId must match [A-Za-z0-9_-]{1,128}')
  }
  return value
}

function requireDimensions(cols, rows) {
  if (!Number.isSafeInteger(cols) || cols < 2 || !Number.isSafeInteger(rows) || rows < 1) {
    throw new RpcError('jsonrpc/invalid-params', 'cols and rows must be positive integers')
  }
}

function integer(value, label) {
  if (!Number.isSafeInteger(value) || value < 0) throw new Error(`${label} must be a non-negative integer`)
  return value
}

function sanitizeInfo(info) {
  if (info === null || typeof info !== 'object') throw new Error('terminal info is missing')
  const shell = info.shell ?? {}
  return {
    id: String(info.id),
    title: String(info.title ?? ''),
    shell: {
      path: String(shell.path ?? ''),
      args: Array.isArray(shell.args) ? shell.args.map(String) : [],
      name: String(shell.name ?? ''),
    },
    cwd: String(info.cwd ?? ''),
    cols: Number(info.cols ?? 0),
    rows: Number(info.rows ?? 0),
    state: info.state === 'exited' || info.state === 'failed' ? info.state : 'running',
    exitCode: Number.isSafeInteger(info.exitCode) ? info.exitCode : null,
    ...(typeof info.error === 'string' ? { error: info.error } : {}),
  }
}

function mapRemoteError(error, action) {
  if (error instanceof RpcError) return error
  const code = typeof error?.code === 'string' ? REMOTE_ERRORS.get(error.code) : undefined
  const message = sanitizeMessage(error)
  if (code !== undefined) return new RpcError(code, `could not ${action}: ${message}`, error?.details)
  if (error?.name === 'AbortError' || error?.code === 'ABORT_ERR') {
    return new RpcError('nano/cancelled', `${action} was cancelled`)
  }
  return new RpcError('nano/terminal/failed', `could not ${action}: ${message}`)
}

function sanitizeMessage(error) {
  const text = error instanceof Error ? error.message : String(error)
  // Never forward screen contents or input through an error message.
  return text.replaceAll(/\u001b\[[0-9;?]*[ -/]*[@-~]/gu, '').slice(0, 300) || 'official terminal service failed'
}

function describe(error) {
  return error instanceof Error ? error.message : String(error)
}

function delay(ms) {
  return new Promise((resolve) => setTimeout(resolve, ms))
}

/** Error names exposed for tests. */
export const TERMINAL_ERROR_NAMES = Object.freeze(
  [...REMOTE_ERRORS.values()].concat('nano/terminal/failed', 'nano/terminal/not-found'),
)

// Keep the imported code map exercised so a typo in a contract name fails at
// module load rather than at the first close.
for (const name of TERMINAL_ERROR_NAMES) {
  if (!ERROR_CODES.has(name)) throw new Error(`unknown terminal error name: ${name}`)
}
