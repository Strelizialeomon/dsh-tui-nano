// NDJSON framing and frame validation for the Nano protocol.
// Both ends read the same contract file; this module enforces the byte-level
// rules that the contract describes but cannot express as plain data.
import { CONTRACT, ERROR_CODES, EVENT_KINDS, LIMITS } from '../../protocol/index.mjs'
import { ProtocolViolation } from './errors.mjs'

const decoder = new TextDecoder('utf-8', { fatal: true })

/** Split a byte stream into complete UTF-8 NDJSON lines without unbounded buffering. */
export class FrameSplitter {
  constructor({ maxFrameBytes = LIMITS.maxFrameBytes } = {}) {
    this.maxFrameBytes = maxFrameBytes
    this.parts = []
    this.pendingBytes = 0
    this.finished = false
  }

  /**
   * Feed one transport chunk.
   * @param {Buffer} chunk - arbitrary byte slice; UTF-8 sequences may span chunks.
   * @returns {string[]} complete frames in arrival order.
   * @throws {ProtocolViolation} when a frame exceeds the limit or is not UTF-8.
   */
  push(chunk) {
    if (this.finished) throw new ProtocolViolation('frame splitter is finished')
    const frames = []
    let offset = 0
    while (offset < chunk.length) {
      const newline = chunk.indexOf(0x0a, offset)
      if (newline === -1) {
        const rest = chunk.subarray(offset)
        this.pendingBytes += rest.length
        if (this.pendingBytes > this.maxFrameBytes) {
          throw new ProtocolViolation(
            `frame exceeds ${String(this.maxFrameBytes)} bytes before newline`,
            ERROR_CODES.get('nano/frame-too-large'),
          )
        }
        this.parts.push(rest)
        return frames
      }
      const part = chunk.subarray(offset, newline)
      const total = this.pendingBytes + part.length
      if (total > this.maxFrameBytes) {
        throw new ProtocolViolation(
          `frame exceeds ${String(this.maxFrameBytes)} bytes`,
          ERROR_CODES.get('nano/frame-too-large'),
        )
      }
      this.parts.push(part)
      const line = Buffer.concat(this.parts, total)
      this.parts = []
      this.pendingBytes = 0
      let text
      try {
        text = decoder.decode(line)
      } catch {
        throw new ProtocolViolation('frame is not valid UTF-8')
      }
      if (text.trim() === '') throw new ProtocolViolation('empty frame')
      frames.push(text)
      offset = newline + 1
    }
    return frames
  }

  /** Assert the stream did not end in the middle of a frame. */
  end() {
    this.finished = true
    if (this.pendingBytes !== 0) throw new ProtocolViolation('stream ended inside a frame')
  }
}

/**
 * Parse and validate one NDJSON line as a JSON-RPC frame.
 * @param {string} line - one complete JSON text.
 * @param {{requireNonce?: boolean, nonce?: string}} [options] - fallback transport nonce rules.
 * @returns {{type: 'request', id: string, method: string, params: object}
 *   | {type: 'notification', method: string, params: object}
 *   | {type: 'response', id: string|null, result?: unknown, error?: object}} the parsed frame.
 */
export function parseFrameLine(line, { requireNonce = false, nonce } = {}) {
  let value
  try {
    value = JSON.parse(line)
  } catch {
    throw new ProtocolViolation('invalid JSON', ERROR_CODES.get('jsonrpc/parse-error'))
  }
  if (value === null || typeof value !== 'object' || Array.isArray(value)) {
    throw new ProtocolViolation('frame must be a JSON object', ERROR_CODES.get('jsonrpc/invalid-request'))
  }
  if (requireNonce && value.nonce !== nonce) {
    throw new ProtocolViolation('frame nonce does not match this generation')
  }
  if (value.jsonrpc !== '2.0') {
    throw new ProtocolViolation('jsonrpc must be "2.0"', ERROR_CODES.get('jsonrpc/invalid-request'))
  }

  if (typeof value.method === 'string') {
    if (!value.method.startsWith('nano.')) {
      throw withRequestId(
        new ProtocolViolation(`method is outside the nano namespace: ${value.method}`, ERROR_CODES.get('jsonrpc/method-not-found')),
        value,
      )
    }
    if (!isPlainObject(value.params)) {
      throw withRequestId(
        new ProtocolViolation('params must be an object', ERROR_CODES.get('jsonrpc/invalid-params')),
        value,
      )
    }
    if (Object.hasOwn(value, 'id')) {
      if (typeof value.id !== 'string' || value.id === '') {
        throw new ProtocolViolation('request id must be a non-empty string', ERROR_CODES.get('jsonrpc/invalid-request'))
      }
      return { type: 'request', id: value.id, method: value.method, params: value.params }
    }
    return { type: 'notification', method: value.method, params: value.params }
  }

  if (Object.hasOwn(value, 'id')) {
    const id = value.id
    if (id !== null && (typeof id !== 'string' || id === '')) {
      throw new ProtocolViolation('response id must be a non-empty string or null', ERROR_CODES.get('jsonrpc/invalid-request'))
    }
    const hasResult = Object.hasOwn(value, 'result')
    const hasError = Object.hasOwn(value, 'error')
    if (hasResult === hasError) {
      throw new ProtocolViolation('response must contain exactly one of result or error', ERROR_CODES.get('jsonrpc/invalid-request'))
    }
    if (hasError) {
      validateErrorObject(value.error)
      return { type: 'response', id, error: value.error }
    }
    return { type: 'response', id, result: value.result }
  }

  throw new ProtocolViolation('frame is neither request, notification, nor response', ERROR_CODES.get('jsonrpc/invalid-request'))
}

/**
 * Validate `nano.hello` params.
 * @param {unknown} params - notification params.
 * @returns {object} a shallow copy with validated fields.
 * @throws {ProtocolViolation} on a protocol-incompatible or malformed hello.
 */
export function validateHello(params) {
  if (!isPlainObject(params)) throw new ProtocolViolation('hello params must be an object')
  const { protocol, runtimeId, generation, versions, cwd, rootSessionId, session, capabilities, limits } = params
  if (!isPlainObject(protocol) || !Number.isInteger(protocol.major) || !Number.isInteger(protocol.minor)) {
    throw new ProtocolViolation('hello.protocol must carry integer major/minor')
  }
  if (protocol.major !== CONTRACT.protocol.major) {
    throw new ProtocolViolation(`unsupported protocol major ${String(protocol.major)}`)
  }
  if (protocol.minor < 0) throw new ProtocolViolation('hello.protocol.minor must not be negative')
  requireNonEmptyString(runtimeId, 'hello.runtimeId')
  requirePositiveInteger(generation, 'hello.generation')
  if (!isPlainObject(versions)) throw new ProtocolViolation('hello.versions must be an object')
  for (const key of ['official', 'bridge', 'node']) requireNonEmptyString(versions[key], `hello.versions.${key}`)
  requireNonEmptyString(cwd, 'hello.cwd')
  requireNonEmptyString(rootSessionId, 'hello.rootSessionId')
  if (!isPlainObject(session)) throw new ProtocolViolation('hello.session must be an object')
  if (session.mode !== 'created' && session.mode !== 'resumed') {
    throw new ProtocolViolation('hello.session.mode must be created or resumed')
  }
  requireNonNegativeInteger(session.seq, 'hello.session.seq')
  if (session.header !== undefined && !isPlainObject(session.header)) {
    throw new ProtocolViolation('hello.session.header must be an object when present')
  }
  if (params.attachmentLimits !== undefined) {
    const limits = params.attachmentLimits
    if (!isPlainObject(limits)) throw new ProtocolViolation('hello.attachmentLimits must be an object')
    for (const key of [
      'maxImageBytes',
      'maxImagesPerMessage',
      'maxMessageImageBytes',
      'maxImagePixels',
      'maxImageDimension',
      'maxChunkBytes',
    ]) {
      requirePositiveInteger(limits[key], `hello.attachmentLimits.${key}`)
    }
    if (!Array.isArray(limits.mediaTypes) || limits.mediaTypes.some((type) => typeof type !== 'string')) {
      throw new ProtocolViolation('hello.attachmentLimits.mediaTypes must be an array of strings')
    }
    if (limits.fileAdmission !== 'verbatim' && limits.fileAdmission !== 'unsupported') {
      throw new ProtocolViolation('hello.attachmentLimits.fileAdmission must be verbatim or unsupported')
    }
  }
  if (!isPlainObject(capabilities)) throw new ProtocolViolation('hello.capabilities must be an object')
  for (const [name, entry] of Object.entries(capabilities)) {
    if (!isPlainObject(entry)) throw new ProtocolViolation(`capability ${name} must be an object`)
    if (!['supported', 'unavailable', 'unsupported'].includes(entry.status)) {
      throw new ProtocolViolation(`capability ${name} has unknown status ${String(entry.status)}`)
    }
    if (entry.mode !== undefined && typeof entry.mode !== 'string') {
      throw new ProtocolViolation(`capability ${name}.mode must be a string`)
    }
    if (entry.detail !== undefined && typeof entry.detail !== 'string') {
      throw new ProtocolViolation(`capability ${name}.detail must be a string`)
    }
    if (!KNOWN_CAPABILITIES.has(name) && entry.required === true && entry.status === 'supported') {
      throw new ProtocolViolation(`unknown required capability ${name}`)
    }
  }
  if (!isPlainObject(limits)) throw new ProtocolViolation('hello.limits must be an object')
  for (const key of ['maxFrameBytes', 'maxPendingRequests', 'maxAttachmentChunkBytes']) {
    if (limits[key] !== LIMITS[key]) throw new ProtocolViolation(`hello.limits.${key} does not match the contract`)
  }
  return params
}

/** Validate a `nano.event` params object. */
export function validateEvent(params, { runtimeId, generation } = {}) {
  if (!isPlainObject(params)) throw new ProtocolViolation('event params must be an object')
  requireNonEmptyString(params.runtimeId, 'event.runtimeId')
  if (runtimeId !== undefined && params.runtimeId !== runtimeId) throw new ProtocolViolation('event runtimeId does not match this host')
  requirePositiveInteger(params.generation, 'event.generation')
  if (generation !== undefined && params.generation !== generation) {
    throw new ProtocolViolation('event generation does not match this generation', ERROR_CODES.get('nano/generation-mismatch'))
  }
  requireNonEmptyString(params.subscriptionId, 'event.subscriptionId')
  requireNonNegativeInteger(params.sequence, 'event.sequence')
  const kind = EVENT_KINDS.get(params.kind)
  if (kind === undefined) throw new ProtocolViolation(`unknown event kind ${String(params.kind)}`)
  if (!isPlainObject(params.payload)) throw new ProtocolViolation('event.payload must be an object')
  return { kind: kind.kind, payload: params.payload }
}

/** Validate a `nano.streamEnded` params object. */
export function validateStreamEnded(params, { runtimeId, generation } = {}) {
  if (!isPlainObject(params)) throw new ProtocolViolation('streamEnded params must be an object')
  requireNonEmptyString(params.runtimeId, 'streamEnded.runtimeId')
  if (runtimeId !== undefined && params.runtimeId !== runtimeId) throw new ProtocolViolation('streamEnded runtimeId does not match this host')
  requirePositiveInteger(params.generation, 'streamEnded.generation')
  if (generation !== undefined && params.generation !== generation) {
    throw new ProtocolViolation('streamEnded generation does not match this generation', ERROR_CODES.get('nano/generation-mismatch'))
  }
  requireNonEmptyString(params.subscriptionId, 'streamEnded.subscriptionId')
  if (!['completed', 'cancelled', 'failed'].includes(params.outcome)) {
    throw new ProtocolViolation('streamEnded.outcome must be completed, cancelled, or failed')
  }
  if (params.reason !== undefined && typeof params.reason !== 'string') {
    throw new ProtocolViolation('streamEnded.reason must be a string')
  }
  return params
}

/** Validate a `nano.startupError` params object. */
export function validateStartupError(params) {
  if (!isPlainObject(params)) throw new ProtocolViolation('startupError params must be an object')
  if (!CONTRACT.startupErrorCodes.includes(params.code)) {
    throw new ProtocolViolation(`unknown startup error code ${String(params.code)}`)
  }
  requireNonEmptyString(params.message, 'startupError.message')
  if (params.holderPid !== undefined) requirePositiveInteger(params.holderPid, 'startupError.holderPid')
  if (params.generation !== undefined) requirePositiveInteger(params.generation, 'startupError.generation')
  if (params.repairPath !== undefined) requireNonEmptyString(params.repairPath, 'startupError.repairPath')
  return params
}

/**
 * Encode one frame as a newline-terminated Buffer.
 * @param {object} frame - JSON-RPC frame.
 * @param {{nonce?: string, requireNonce?: boolean}} [options] - fallback transport rules.
 * @returns {Buffer} UTF-8 bytes including the trailing newline.
 */
export function encodeFrame(frame, { nonce, requireNonce = false } = {}) {
  const value = requireNonce ? { ...frame, nonce } : frame
  let text
  try {
    text = JSON.stringify(value)
  } catch (error) {
    throw new ProtocolViolation(`frame is not JSON-serializable: ${String(error)}`)
  }
  const bytes = Buffer.byteLength(text, 'utf8')
  if (bytes > LIMITS.maxFrameBytes) {
    throw new ProtocolViolation(`frame is ${String(bytes)} bytes, over the ${String(LIMITS.maxFrameBytes)} limit`, ERROR_CODES.get('nano/frame-too-large'))
  }
  return Buffer.concat([Buffer.from(text, 'utf8'), Buffer.from('\n')])
}

function validateErrorObject(error) {
  if (!isPlainObject(error)) throw new ProtocolViolation('error must be an object', ERROR_CODES.get('jsonrpc/invalid-request'))
  if (!Number.isInteger(error.code)) throw new ProtocolViolation('error.code must be an integer', ERROR_CODES.get('jsonrpc/invalid-request'))
  if (typeof error.message !== 'string' || error.message === '') {
    throw new ProtocolViolation('error.message must be a non-empty string', ERROR_CODES.get('jsonrpc/invalid-request'))
  }
  if (error.data !== undefined && !isPlainObject(error.data)) {
    throw new ProtocolViolation('error.data must be an object when present', ERROR_CODES.get('jsonrpc/invalid-request'))
  }
}

function withRequestId(error, value) {
  if (typeof value.id === 'string' && value.id !== '') error.requestId = value.id
  return error
}

function requireNonEmptyString(value, field) {
  if (typeof value !== 'string' || value === '') throw new ProtocolViolation(`${field} must be a non-empty string`)
}

function requirePositiveInteger(value, field) {
  if (!Number.isInteger(value) || value <= 0) throw new ProtocolViolation(`${field} must be a positive integer`)
}

function requireNonNegativeInteger(value, field) {
  if (!Number.isInteger(value) || value < 0) throw new ProtocolViolation(`${field} must be a non-negative integer`)
}

function isPlainObject(value) {
  return value !== null && typeof value === 'object' && !Array.isArray(value)
}

/** Capability names this generation understands; unknown names are optional minor additions. */
const KNOWN_CAPABILITIES = new Set(CONTRACT.capabilityNames)
