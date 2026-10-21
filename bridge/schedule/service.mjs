// Nano RPC surface for the official schedule service.
//
// This module owns no timing and no task table. It validates the wire shape,
// delegates to `ctx.schedule`, and forwards post-commit `schedule/changed`
// notifications to the Rust client. Delivery, catch-up, conflict detection,
// and history retention all stay inside the official service.
import { RpcError } from '../runtime/errors.mjs'

/** A schedule RPC failure with the official code preserved in `data.code`. */
export function scheduleRpcError(error) {
  if (error instanceof RpcError) return error
  if (error === null || typeof error !== 'object' || typeof error.code !== 'string') {
    return new RpcError('nano/schedule/error', error instanceof Error ? error.message : String(error), {
      code: 'internal_error',
    })
  }
  return new RpcError('nano/schedule/error', `${error.code}: ${typeof error.message === 'string' ? error.message : String(error.code)}`, {
    code: error.code,
  })
}

/** One schedule request handler bound to this host's root session. */
export class ScheduleBridge {
  /**
   * @param {{ctx: object, hub: object, schedule: object, rootSessionId: string}} options - host wiring.
   */
  constructor({ ctx, hub, schedule, rootSessionId }) {
    this.ctx = ctx
    this.hub = hub
    this.schedule = schedule
    this.rootSessionId = rootSessionId
    this.address = { rootSessionId }
    this.closed = false
    this.stopListener = ctx.on('schedule/changed', () => {
      if (!this.closed) this.hub.publish('schedule.changed', { address: this.address })
    })
  }

  /** Stop forwarding changes; the official service stays alive for shutdown. */
  close() {
    this.closed = true
    try {
      this.stopListener?.()
    } catch {
      // listener disposal is best-effort during teardown
    }
  }

  /**
   * Read active and inactive tasks with their session binding and latest receipt.
   * @param {object} params - `address`.
   * @returns {Promise<{address: object, entries: object[]}>} catalog.
   */
  async catalog(params) {
    this.assertAddress(params?.address)
    try {
      return { address: this.address, entries: await this.schedule.catalog() }
    } catch (error) {
      throw scheduleRpcError(error)
    }
  }

  /**
   * Read only active tasks from the official list path.
   * @param {object} params - `address`.
   * @returns {Promise<{address: object, records: object[]}>} active records.
   */
  async list(params) {
    this.assertAddress(params?.address)
    try {
      return { address: this.address, records: await this.schedule.list({ sessionId: this.rootSessionId }) }
    } catch (error) {
      throw scheduleRpcError(error)
    }
  }

  /**
   * Create one task bound to the root session. The official service validates
   * the title, prompt, selector, zone, and cron dialect.
   * @param {object} params - `address`, `request`.
   * @param {AbortSignal} [signal] - request cancellation.
   * @returns {Promise<{address: object, record: object}>} stored record.
   */
  async create(params, signal) {
    this.assertAddress(params?.address)
    const request = params?.request
    if (request === null || typeof request !== 'object' || Array.isArray(request)) {
      throw new RpcError('jsonrpc/invalid-params', 'request must be an object')
    }
    try {
      return { address: this.address, record: await this.schedule.create(this.rootSessionId, request, signal) }
    } catch (error) {
      throw scheduleRpcError(error)
    }
  }

  /**
   * Compare-and-set one task. `expected` is the complete record captured by the
   * client; conflicts and misses are returned as results, not errors.
   * @param {object} params - `address`, `id`, `expected`, optional `title`, `prompt`, `change`.
   * @param {AbortSignal} [signal] - request cancellation.
   * @returns {Promise<{result: object}>} official update result.
   */
  async update(params, signal) {
    this.assertAddress(params?.address)
    const id = params?.id
    if (typeof id !== 'string' || id === '') {
      throw new RpcError('jsonrpc/invalid-params', 'id must be a non-empty string')
    }
    const expected = params?.expected
    if (expected === null || typeof expected !== 'object' || Array.isArray(expected)) {
      throw new RpcError('jsonrpc/invalid-params', 'expected must be the complete observed record')
    }
    const request = { sessionId: this.rootSessionId, id, expected }
    if (params.title !== undefined) request.title = params.title
    if (params.prompt !== undefined) request.prompt = params.prompt
    if (params.change !== undefined) request.change = params.change
    try {
      return { result: await this.schedule.update(request, signal) }
    } catch (error) {
      throw scheduleRpcError(error)
    }
  }

  /**
   * Delete one task and its saved delivery history. Queued messages stay.
   * @param {object} params - `address`, `id`.
   * @param {AbortSignal} [signal] - request cancellation.
   * @returns {Promise<{result: object}>} `{id, deleted}` or `schedule_not_found`.
   */
  async delete(params, signal) {
    this.assertAddress(params?.address)
    const id = params?.id
    if (typeof id !== 'string' || id === '') {
      throw new RpcError('jsonrpc/invalid-params', 'id must be a non-empty string')
    }
    try {
      return { result: await this.schedule.delete({ sessionId: this.rootSessionId, id }, signal) }
    } catch (error) {
      throw scheduleRpcError(error)
    }
  }

  /**
   * Read one bounded history page. The official service rejects an invalid
   * limit with `invalid_rule`, which keeps the acceptance code visible.
   * @param {object} params - `address`, `id`, `limit`, optional `before`.
   * @returns {Promise<{result: object}>} history page or lookup failure.
   */
  async history(params) {
    this.assertAddress(params?.address)
    const id = params?.id
    if (typeof id !== 'string' || id === '') {
      throw new RpcError('jsonrpc/invalid-params', 'id must be a non-empty string')
    }
    const request = { sessionId: this.rootSessionId, id, limit: params?.limit }
    if (params.before !== undefined) request.before = params.before
    try {
      return { result: await this.schedule.history(request) }
    } catch (error) {
      throw scheduleRpcError(error)
    }
  }

  assertAddress(address) {
    if (address === null || typeof address !== 'object' || Array.isArray(address)) {
      throw new RpcError('jsonrpc/invalid-params', 'address must be an object')
    }
    if (typeof address.rootSessionId !== 'string' || address.rootSessionId === '') {
      throw new RpcError('jsonrpc/invalid-params', 'address.rootSessionId must be a non-empty string')
    }
    if (address.rootSessionId !== this.rootSessionId) {
      throw new RpcError('nano/unsupported', 'address names a root session this host does not serve')
    }
    if (address.childSessionId !== undefined) {
      throw new RpcError('nano/unsupported', 'child-session addresses belong to S12 and are not served by this generation')
    }
  }
}
