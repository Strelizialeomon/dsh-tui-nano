// Shared, dependency-free access to the Nano protocol contract.
// The JSON file is the source of truth; this module only freezes convenient views of it.
import { readFileSync } from 'node:fs'

/** Parsed and deep-frozen protocol contract. */
export const CONTRACT = deepFreeze(JSON.parse(readFileSync(new URL('./nano-protocol.json', import.meta.url), 'utf8')))

/** Wire protocol version the host and Rust runtime must agree on. */
export const PROTOCOL = CONTRACT.protocol

/** Fixed limits shared by both implementations. */
export const LIMITS = CONTRACT.limits

/** JSON-RPC and Nano-specific error codes. */
export const ERROR_CODES = new Map(CONTRACT.errorCodes.map((entry) => [entry.name, entry.code]))

/** Methods implemented by this host generation, keyed by method name. */
export const IMPLEMENTED_METHODS = new Map(
  CONTRACT.methods.filter((method) => method.status === 'implemented').map((method) => [method.name, method]),
)

/** Methods reserved for a later spec, keyed by method name. */
export const RESERVED_METHODS = new Map(
  CONTRACT.methods.filter((method) => method.status === 'reserved').map((method) => [method.name, method]),
)

/** Event kinds this host may deliver. */
export const EVENT_KINDS = new Map(CONTRACT.eventKinds.map((entry) => [entry.kind, entry]))

/** Set of allowed lifecycle transitions as `from->to` strings. */
export const LIFECYCLE_EDGES = new Set(CONTRACT.lifecycle.transitions.map(([from, to]) => `${from}->${to}`))

/**
 * Freeze every nested object and array so a consumer cannot mutate the shared contract.
 * @param {unknown} value - parsed JSON value.
 * @returns {unknown} the same value, deeply frozen.
 */
function deepFreeze(value) {
  if (value === null || typeof value !== 'object' || Object.isFrozen(value)) return value
  for (const child of Object.values(value)) deepFreeze(child)
  return Object.freeze(value)
}
