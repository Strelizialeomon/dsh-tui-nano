// Shared error types for the bridge. Codes come from the protocol contract.
import { ERROR_CODES } from '../../protocol/index.mjs'

/** A startup failure that must be reported through `nano.startupError`. */
export class StartupError extends Error {
  /**
   * @param {'occupied'|'startup-failed'|'cleanup-failed'|'unsupported'} code - startup error code.
   * @param {string} message - sanitized, user-facing description.
   * @param {{holderPid?: number, generation?: number, repairPath?: string, cause?: unknown}} [extra] - structured details.
   */
  constructor(code, message, extra = {}) {
    super(message, extra.cause === undefined ? undefined : { cause: extra.cause })
    this.name = 'StartupError'
    this.code = code
    this.holderPid = extra.holderPid
    this.generation = extra.generation
    this.repairPath = extra.repairPath
  }
}

/** A JSON-RPC method error returned to the caller. */
export class RpcError extends Error {
  /**
   * @param {string} name - contract error name, such as `nano/not-ready`.
   * @param {string} message - description.
   * @param {object} [data] - optional structured details.
   */
  constructor(name, message, data) {
    super(message)
    const code = ERROR_CODES.get(name)
    if (code === undefined) throw new Error(`unknown protocol error name: ${name}`)
    this.name = 'RpcError'
    this.rpcName = name
    this.code = code
    this.data = data
  }
}

/** A transport-level violation that ends the connection. */
export class ProtocolViolation extends Error {
  /**
   * @param {string} message - what is wrong with the byte stream or frame.
   * @param {number} [code] - JSON-RPC code to report when an id is available.
   */
  constructor(message, code = ERROR_CODES.get('nano/protocol')) {
    super(message)
    this.name = 'ProtocolViolation'
    this.code = code
    /** Request id when the frame was still identifiable. */
    this.requestId = undefined
  }
}

/** Convert any thrown value into a JSON-RPC error frame body. */
export function toRpcErrorBody(error) {
  if (error instanceof RpcError) {
    return error.data === undefined
      ? { code: error.code, message: error.message }
      : { code: error.code, message: error.message, data: error.data }
  }
  if (error instanceof ProtocolViolation) {
    return { code: error.code, message: error.message }
  }
  return { code: ERROR_CODES.get('jsonrpc/internal-error'), message: 'internal error' }
}
