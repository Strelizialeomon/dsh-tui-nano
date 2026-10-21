// Attachment host for S06: chunked intake, session-bound file receipts, image
// staging, and the official prompt admission call.
//
// Images are never persisted here: the staged source bytes are handed to
// `ctx.attachments.admitPromptContent()` at submit time, which validates,
// normalizes and persists them in input order. Generic files are spooled to a
// staging file and then committed through `ctx.attachments.saveFileStream()`;
// the returned `FileAttachmentRef` is the opaque receipt the client can cite
// once. The bridge owns receipt binding and retirement because the minimal
// Nano profile does not mount the browser file-upload service.
import fs from 'node:fs'
import path from 'node:path'
import { randomUUID } from 'node:crypto'
import { LIMITS } from '../../protocol/index.mjs'
import { RpcError } from '../runtime/errors.mjs'

const IMAGE_MEDIA_TYPES = Object.freeze(['image/png', 'image/jpeg', 'image/webp', 'image/gif'])
const MAX_RECEIPT_ID_LENGTH = 128
const MAX_NAME_LENGTH = 255
const MAX_FILE_REFERENCES = 50
const FALLBACK_IMAGE_MAX_BYTES = 64 * 1024 * 1024

/** Official attachment failure codes that are caller-correctable image refusals. */
const IMAGE_ADMISSION_CODES = new Set([
  'TOO_MANY_IMAGES',
  'IMAGES_TOO_LARGE',
  'UNSUPPORTED_IMAGE_TYPE',
  'INVALID_IMAGE_BASE64',
  'INVALID_IMAGE',
  'IMAGE_TYPE_MISMATCH',
  'IMAGE_TOO_LARGE',
  'IMAGE_TOO_MANY_PIXELS',
  'IMAGE_DIMENSION_TOO_LARGE',
])

/**
 * One attachment host per host generation, bound to the root Agent.
 */
export class AttachmentService {
  /**
   * @param {{
   *   ctx: object,
   *   agent: object,
   *   session: object,
   *   rootSessionId: string,
   *   dataRoot: string,
   *   config: object,
   * }} options - host wiring.
   */
  constructor({ ctx, agent, session, rootSessionId, dataRoot, config }) {
    this.ctx = ctx
    this.agent = agent
    this.session = session
    this.rootSessionId = rootSessionId
    this.dataRoot = dataRoot
    this.config = config
    this.address = { rootSessionId }
    this.receipts = new Map()
    this.pending = new Map()
    this.closed = false
    this.stagingRoot = path.join(dataRoot, 'runtime', 'attachment-staging')
  }

  /** Deployment image limits from the official store, when composed. */
  get imageLimits() {
    const store = this.ctx.get('attachments')
    if (store === undefined || store.imageLimits === undefined) return undefined
    return store.imageLimits
  }

  /** Whether any official attachment store is composed. */
  get supported() {
    return this.ctx.get('attachments') !== undefined
  }

  /** Optional `nano.hello` attachment limit block. */
  helloLimits() {
    const limits = this.imageLimits
    if (limits === undefined) return undefined
    const numeric = [
      'maxImageBytes',
      'maxImagesPerMessage',
      'maxMessageImageBytes',
      'maxImagePixels',
      'maxImageDimension',
    ]
    if (!numeric.every((key) => Number.isSafeInteger(limits[key]) && limits[key] > 0)) {
      return undefined
    }
    if (!Array.isArray(limits.mediaTypes)) return undefined
    const mediaTypes = limits.mediaTypes.filter((type) => typeof type === 'string')
    if (mediaTypes.length === 0) return undefined
    return {
      maxImageBytes: limits.maxImageBytes,
      maxImagesPerMessage: limits.maxImagesPerMessage,
      maxMessageImageBytes: limits.maxMessageImageBytes,
      maxImagePixels: limits.maxImagePixels,
      maxImageDimension: limits.maxImageDimension,
      mediaTypes,
      maxChunkBytes: LIMITS.maxAttachmentChunkBytes,
      fileAdmission: 'verbatim',
    }
  }

  /**
   * Start one staged upload. `receiptId` is the client's business identity for
   * this attachment; the same id cannot be reused after it is committed.
   * @param {object} params - `address`, `receiptId`, `kind`, optional `name`, `mediaType`, `bytes`.
   * @returns {{receiptId: string, kind: string, ready: false}} begin receipt.
   */
  begin(params) {
    this.assertOpen()
    const kind = params?.kind
    if (kind !== 'image' && kind !== 'file') {
      throw new RpcError('jsonrpc/invalid-params', 'kind must be image or file')
    }
    this.assertAddress(params?.address, kind)
    if (!this.supported) {
      throw new RpcError('nano/unsupported', 'the official attachment store is not composed in this profile')
    }
    const receiptId = requireString(params?.receiptId, 'receiptId', MAX_RECEIPT_ID_LENGTH)
    if (this.receipts.has(receiptId)) {
      throw new RpcError('nano/attachments/receipt-committed', `receipt ${receiptId} was already used; use a new receipt for a new attachment`)
    }
    const name = optionalName(params?.name)
    const declaredBytes = optionalNonNegativeInteger(params?.bytes, 'bytes')
    const receipt = {
      id: receiptId,
      kind,
      name,
      address: params.address,
      declaredBytes,
      received: 0,
      sequence: 0,
      chunks: [],
      fd: undefined,
      tempPath: undefined,
      state: 'staging',
      ref: undefined,
      mediaType: undefined,
    }
    if (kind === 'image') {
      const mediaType = params?.mediaType
      if (typeof mediaType !== 'string' || !IMAGE_MEDIA_TYPES.includes(mediaType)) {
        throw new RpcError('jsonrpc/invalid-params', 'image mediaType must be a supported image type')
      }
      const limits = this.imageLimits
      if (limits !== undefined && !limits.mediaTypes.includes(mediaType)) {
        throw new RpcError('nano/attachments/image-invalid', `media type ${mediaType} is not accepted by this deployment`, {
          reason: 'UNSUPPORTED_IMAGE_TYPE',
        })
      }
      const declaredLimit = limits?.maxImageBytes ?? FALLBACK_IMAGE_MAX_BYTES
      if (declaredBytes !== undefined && declaredBytes > declaredLimit) {
        throw new RpcError('nano/attachments/image-invalid', `image is ${declaredBytes} bytes, over the ${declaredLimit} byte limit`, {
          reason: 'IMAGE_TOO_LARGE',
        })
      }
      receipt.mediaType = mediaType
    } else {
      try {
        fs.mkdirSync(this.stagingRoot, { recursive: true, mode: 0o700 })
        fs.chmodSync(this.stagingRoot, 0o700)
        // The receipt id is client-controlled; never use it as a path component.
        receipt.tempPath = path.join(this.stagingRoot, `${randomUUID()}.part`)
        receipt.fd = fs.openSync(receipt.tempPath, 'wx', 0o600)
      } catch {
        throw new RpcError('nano/attachments/file-failed', 'could not stage the upload')
      }
    }
    this.receipts.set(receiptId, receipt)
    return { receiptId, kind, ready: false }
  }

  /**
   * Append one ordered chunk.
   * @param {object} params - `receiptId`, `sequence`, canonical base64 `bytes`.
   * @returns {{receiptId: string, received: number, nextSequence: number}} acknowledgement.
   */
  chunk(params) {
    this.assertOpen()
    const receipt = this.requireReceipt(params?.receiptId)
    if (receipt.state !== 'staging') {
      throw new RpcError('nano/attachments/receipt-incomplete', `receipt ${receipt.id} is not accepting chunks`)
    }
    if (!Number.isInteger(params?.sequence) || params.sequence !== receipt.sequence) {
      throw new RpcError('nano/attachments/chunk-sequence', `expected chunk sequence ${receipt.sequence}`, {
        expected: receipt.sequence,
      })
    }
    const bytes = decodeCanonicalBase64(params?.bytes)
    if (bytes.length === 0) {
      throw new RpcError('jsonrpc/invalid-params', 'chunk must carry at least one byte')
    }
    if (bytes.length > LIMITS.maxAttachmentChunkBytes) {
      throw new RpcError('nano/frame-too-large', `chunk is ${bytes.length} raw bytes, over the ${LIMITS.maxAttachmentChunkBytes} byte limit`)
    }
    const next = receipt.received + bytes.length
    if (receipt.declaredBytes !== undefined && next > receipt.declaredBytes) {
      throw this.kindError(receipt, `upload exceeds the declared ${receipt.declaredBytes} bytes`)
    }
    if (receipt.kind === 'image') {
      const limit = this.imageLimits?.maxImageBytes ?? FALLBACK_IMAGE_MAX_BYTES
      if (next > limit) {
        throw new RpcError('nano/attachments/image-invalid', `image is over the ${limit} byte limit`, {
          reason: 'IMAGE_TOO_LARGE',
        })
      }
      receipt.chunks.push(bytes)
    } else {
      try {
        fs.writeSync(receipt.fd, bytes)
      } catch {
        receipt.state = 'failed'
        this.cleanup(receipt)
        throw new RpcError('nano/attachments/file-failed', 'could not stage the upload')
      }
    }
    receipt.received = next
    receipt.sequence += 1
    return { receiptId: receipt.id, received: receipt.received, nextSequence: receipt.sequence }
  }

  /**
   * Finish one upload. Images remain staged until prompt admission; generic
   * files are committed verbatim now and return their durable reference.
   * @param {object} params - `receiptId`.
   * @returns {{receiptId: string, kind: string, ready: true, file?: object, image?: object}} receipt.
   */
  async finish(params) {
    this.assertOpen()
    const receipt = this.requireReceipt(params?.receiptId)
    if (receipt.state !== 'staging') {
      throw new RpcError('nano/attachments/receipt-incomplete', `receipt ${receipt.id} is not awaiting finish`)
    }
    // A zero-byte generic file is valid; an image still fails official
    // admission. Every other empty upload is incomplete.
    if (receipt.sequence === 0 && receipt.declaredBytes !== 0) {
      throw new RpcError('nano/attachments/receipt-incomplete', 'upload has no bytes')
    }
    if (receipt.declaredBytes !== undefined && receipt.received !== receipt.declaredBytes) {
      throw new RpcError(
        'nano/attachments/receipt-incomplete',
        `received ${receipt.received} of ${receipt.declaredBytes} declared bytes`,
      )
    }
    if (receipt.kind === 'image') {
      receipt.data = Buffer.concat(receipt.chunks)
      receipt.chunks = []
      receipt.state = 'ready'
      return {
        receiptId: receipt.id,
        kind: 'image',
        ready: true,
        image: { mediaType: receipt.mediaType, bytes: receipt.received },
      }
    }
    try {
      fs.closeSync(receipt.fd)
      receipt.fd = undefined
    } catch {
      receipt.state = 'failed'
      this.cleanup(receipt)
      throw new RpcError('nano/attachments/file-failed', 'could not close the staged upload')
    }
    const store = this.ctx.get('attachments')
    const stream = fs.createReadStream(receipt.tempPath)
    // The consumer owns the bytes; this listener only prevents an unhandled
    // stream error when the official save rejects and the staging file is
    // removed before the stream opens.
    stream.on('error', () => {})
    try {
      const ref = await store.saveFileStream({
        data: stream,
        name: receipt.name,
      })
      receipt.ref = ref
      receipt.state = 'ready'
      this.removeTemp(receipt)
      return {
        receiptId: receipt.id,
        kind: 'file',
        ready: true,
        file: { attachmentId: String(ref.attachmentId), name: ref.name, bytes: ref.bytes },
      }
    } catch (error) {
      receipt.state = 'failed'
      stream.destroy()
      this.cleanup(receipt)
      throw this.mapAttachmentError(error, 'file')
    }
  }

  /**
   * Drop one local card. A committed receipt cannot be revoked; a staged file
   * that was already saved is left in official storage and only the receipt is
   * dropped.
   * @param {object} params - `receiptId`.
   * @returns {{discarded: boolean}} whether the staging was removed.
   */
  discard(params) {
    this.assertOpen()
    const receipt = this.receipts.get(params?.receiptId)
    if (receipt === undefined) return { discarded: true }
    if (receipt.state === 'committed' || receipt.state === 'bound') {
      return { discarded: false }
    }
    this.cleanup(receipt)
    this.receipts.delete(receipt.id)
    return { discarded: true }
  }

  /**
   * Resolve staged blocks into official admitted content. Returns the ordered
   * content blocks for the Agent message and the receipts the caller must
   * commit or roll back after `followup`/`steer`.
   * @param {object} address - wire address.
   * @param {unknown} blocks - ordered wire content blocks.
   * @returns {Promise<{content: object[], receipts: object[]}>} admission result.
   */
  async admitContent(address, blocks) {
    this.assertOpen()
    if (!Array.isArray(blocks) || blocks.length === 0) {
      throw new RpcError('nano/conversation/empty', 'content must be a non-empty array')
    }
    if (!this.supported) {
      throw new RpcError('nano/unsupported', 'the official attachment store is not composed in this profile')
    }
    this.assertAddress(
      address,
      blocks.some((block) => block?.type === 'file') ? 'file' : 'image',
    )
    const admission = []
    const receipts = []
    let hasText = false
    let imageCount = 0
    let imageBytes = 0
    for (const block of blocks) {
      if (block === null || typeof block !== 'object' || Array.isArray(block)) {
        throw new RpcError('nano/conversation/content', 'each content block must be an object')
      }
      if (block.type === 'text') {
        if (typeof block.text !== 'string') {
          throw new RpcError('nano/conversation/content', 'text blocks must carry a string')
        }
        if (block.text.trim() !== '') hasText = true
        admission.push({ type: 'text', text: block.text })
        continue
      }
      if (block.type !== 'image' && block.type !== 'file') {
        throw new RpcError('nano/conversation/content', `content block type ${String(block.type)} is not supported`)
      }
      const receipt = this.requireReceipt(block.receiptId)
      const kind = block.type
      if (receipt.kind !== kind) {
        throw new RpcError('nano/conversation/content', `receipt ${receipt.id} is a ${receipt.kind}, not a ${kind}`)
      }
      this.assertSameAddress(receipt.address, address)
      if (receipt.state === 'staging') {
        throw new RpcError('nano/attachments/receipt-incomplete', `receipt ${receipt.id} has not finished uploading`)
      }
      if (receipt.state === 'committed' || receipt.state === 'bound') {
        throw new RpcError(
          'nano/attachments/receipt-committed',
          `receipt ${receipt.id} is already committed; it cannot be submitted again`,
        )
      }
      if (receipt.state !== 'ready') {
        throw new RpcError('nano/attachments/receipt-incomplete', `receipt ${receipt.id} is not ready`)
      }
      if (kind === 'image') {
        imageCount += 1
        imageBytes += receipt.data.length
        admission.push({ type: 'image', mediaType: receipt.mediaType, data: receipt.data.toString('base64') })
      } else {
        admission.push({ type: 'file', attachment: receipt.ref })
      }
      receipts.push(receipt)
    }
    if (!hasText && receipts.length === 0) {
      throw new RpcError('nano/conversation/empty', 'content needs non-whitespace text or an attachment')
    }
    const limits = this.imageLimits
    if (limits !== undefined && imageCount > 0) {
      if (imageCount > limits.maxImagesPerMessage) {
        throw new RpcError('nano/attachments/limit', `message has ${imageCount} images, over the ${limits.maxImagesPerMessage} image limit`)
      }
      if (imageBytes > limits.maxMessageImageBytes) {
        throw new RpcError('nano/attachments/limit', `message images total ${imageBytes} bytes, over the ${limits.maxMessageImageBytes} byte limit`)
      }
      if (!(await this.modelSupportsImages())) {
        throw new RpcError('nano/attachments/model-no-images', 'the selected model does not accept image input', {
          reason: 'MODEL_DOES_NOT_SUPPORT_IMAGES',
        })
      }
    }

    for (const receipt of receipts) receipt.state = 'bound'
    try {
      const admitted = await this.ctx.get('attachments').admitPromptContent(admission)
      return { content: admitted.map(toContentBlock), receipts }
    } catch (error) {
      for (const receipt of receipts) {
        if (receipt.state === 'bound') receipt.state = 'ready'
      }
      throw this.mapAttachmentError(error, receipts.some((receipt) => receipt.kind === 'image') ? 'image' : 'file')
    }
  }

  /**
   * Bind receipts to the request before the Agent sees the message, so a
   * synchronous inbox insertion can retire them.
   * @param {string} requestId - business identity of the pending prompt.
   * @param {object[]} receipts - receipts returned by `admitContent`.
   */
  bindPrompt(requestId, receipts) {
    if (receipts.length > 0) this.pending.set(requestId, receipts)
  }

  /**
   * Mark the receipts of one accepted request as committed. If the Agent's
   * synchronous inbox event already retired them, do not recreate the pending
   * entry.
   * @param {string} requestId - business identity of the accepted prompt.
   * @param {object[]} receipts - receipts returned by `admitContent`.
   */
  commit(requestId, receipts) {
    for (const receipt of receipts) {
      receipt.state = 'committed'
      if (receipt.kind === 'image') receipt.data = undefined
    }
    if (
      receipts.length > 0 &&
      receipts.every((receipt) => this.receipts.get(receipt.id) === receipt)
    ) {
      this.pending.set(requestId, receipts)
    }
  }

  /**
   * Put receipts back to ready after an Agent rejection before acceptance.
   * @param {string} requestId - business identity being released.
   * @param {object[]} receipts - receipts returned by `admitContent`.
   */
  releasePrompt(requestId, receipts) {
    this.pending.delete(requestId)
    for (const receipt of receipts) {
      if (receipt.state === 'bound' || receipt.state === 'committed') {
        receipt.state = 'ready'
      }
    }
  }

  /**
   * Retire committed receipts once their request is observed in the official
   * inbox projection. Presence means the prompt is safely queued or claimed.
   * @param {object} inbox - official inbox projection.
   */
  observeInbox(inbox) {
    const ids = new Set()
    for (const list of [inbox?.nextTurn, inbox?.nextStep]) {
      if (!Array.isArray(list)) continue
      for (const message of list) {
        const rpcId = rpcIdOf(message)
        if (rpcId !== undefined) ids.add(rpcId)
      }
    }
    for (const requestId of ids) this.retire(requestId)
  }

  /**
   * Retire committed receipts once the durable user message is observed.
   * @param {object} message - official user message value.
   */
  observeUserMessage(message) {
    const rpcId = rpcIdOf(message)
    if (rpcId !== undefined) this.retire(rpcId)
  }

  /** Retire receipts for a queue item the official inbox discarded. */
  observeDiscarded(message) {
    const rpcId = rpcIdOf(message)
    if (rpcId !== undefined) this.retire(rpcId)
  }

  /** Drop one request's committed receipts. */
  retire(requestId) {
    const receipts = this.pending.get(requestId)
    if (receipts === undefined) return
    this.pending.delete(requestId)
    for (const receipt of receipts) {
      receipt.state = 'retired'
      this.receipts.delete(receipt.id)
    }
  }

  /**
   * List official `@file` candidates for the root Agent.
   * @param {object} params - `address`, `query`, optional `limit`.
   * @param {AbortSignal} signal - cancels provider work.
   * @returns {Promise<{candidates: object[]}>} path-only candidates.
   */
  async listReferences(params, signal) {
    this.assertOpen()
    this.assertAddress(params?.address, 'file-reference')
    const query = typeof params?.query === 'string' ? params.query : undefined
    if (query === undefined) {
      throw new RpcError('jsonrpc/invalid-params', 'query must be a string')
    }
    const service = this.ctx.get('fileReferences')
    if (service === undefined) {
      throw new RpcError('nano/unsupported', 'file-reference discovery is not composed in this profile')
    }
    const limit = optionalPositiveInteger(params?.limit, 'limit')
    try {
      const candidates = await service.list(this.agent, query, signal)
      const mapped = candidates
        .filter((candidate) => candidate && typeof candidate.path === 'string')
        .map((candidate) => ({
          path: candidate.path,
          kind: candidate.kind === 'directory' ? 'directory' : 'file',
        }))
      return { candidates: limit === undefined ? mapped : mapped.slice(0, Math.min(limit, MAX_FILE_REFERENCES)) }
    } catch (error) {
      if (signal?.aborted) throw new RpcError('nano/cancelled', 'file-reference request was cancelled')
      throw new RpcError('nano/file-references/failed', 'the file-reference provider failed')
    }
  }

  /** Release all staging. Official saved objects are never deleted. */
  close() {
    if (this.closed) return
    this.closed = true
    for (const receipt of this.receipts.values()) this.cleanup(receipt)
    this.receipts.clear()
    this.pending.clear()
  }

  /**
   * Whether the selected route accepts image input. An absent modality list is
   * unknown, not negative; only an explicit omission rejects before send.
   * @returns {Promise<boolean>} support fact.
   */
  async modelSupportsImages() {
    let selection
    if (typeof this.config.provider === 'string' && typeof this.config.model === 'string') {
      selection = { provider: this.config.provider, model: this.config.model }
    } else {
      const header = this.session.requestHeader?.()
      if (header?.config?.provider !== undefined && header?.config?.model !== undefined) {
        selection = { provider: header.config.provider, model: header.config.model }
      } else {
        const defaults = this.ctx.get('agentDefaultModel')
        if (defaults?.currentSelection !== undefined) {
          const current = defaults.currentSelection()
          if (current?.provider !== undefined && current?.model !== undefined) selection = current
        }
      }
    }
    if (selection === undefined) return true
    const llm = this.ctx.get('llm')
    if (llm?.resolveModelInfo === undefined) return true
    try {
      const info = await llm.resolveModelInfo(selection.provider, selection.model)
      if (info?.inputModalities === undefined) return true
      return info.inputModalities.includes('image')
    } catch {
      // The route may be unavailable for other reasons; official admission
      // still owns the final refusal.
      return true
    }
  }

  /** Validate the wire address and the subagent admission rule. */
  assertAddress(address, kind) {
    if (address === null || typeof address !== 'object' || Array.isArray(address)) {
      throw new RpcError('jsonrpc/invalid-params', 'address must be an object')
    }
    if (typeof address.rootSessionId !== 'string' || address.rootSessionId === '') {
      throw new RpcError('jsonrpc/invalid-params', 'address.rootSessionId must be a non-empty string')
    }
    if (address.rootSessionId !== this.rootSessionId) {
      throw new RpcError('nano/conversation/address', 'address names a root session this host does not serve')
    }
    if (address.childSessionId !== undefined && (kind === 'file' || kind === 'file-reference')) {
      // The official contract accepts only images for subagent addresses.
      // Child routing itself belongs to S12; the file rule is enforced here so
      // a future caller cannot silently offer generic files.
      throw new RpcError('nano/unsupported', 'generic files are rejected for subagent addresses; images only')
    }
  }

  /** The receipt must belong to the exact address being submitted. */
  assertSameAddress(receiptAddress, address) {
    if (receiptAddress?.rootSessionId !== address?.rootSessionId ||
        receiptAddress?.childSessionId !== address?.childSessionId) {
      throw new RpcError('nano/conversation/address', 'receipt belongs to another conversation address')
    }
  }

  requireReceipt(receiptId) {
    if (typeof receiptId !== 'string' || receiptId === '') {
      throw new RpcError('jsonrpc/invalid-params', 'receiptId must be a non-empty string')
    }
    const receipt = this.receipts.get(receiptId)
    if (receipt === undefined) {
      throw new RpcError('nano/attachments/receipt-unknown', `unknown receipt ${receiptId}`)
    }
    return receipt
  }

  kindError(receipt, message) {
    return new RpcError(
      receipt.kind === 'image' ? 'nano/attachments/image-invalid' : 'nano/attachments/limit',
      message,
      receipt.kind === 'image' ? { reason: 'IMAGE_TOO_LARGE' } : undefined,
    )
  }

  mapAttachmentError(error, kind) {
    const store = this.ctx.get('attachments')
    if (store?.isAttachmentError !== undefined && store.isAttachmentError(error)) {
      const code = error.code
      if (IMAGE_ADMISSION_CODES.has(code)) {
        return new RpcError('nano/attachments/image-invalid', error.message, { reason: code })
      }
      return new RpcError(
        kind === 'image' ? 'nano/attachments/image-invalid' : 'nano/attachments/file-failed',
        error.message,
        { reason: code },
      )
    }
    if (error instanceof RpcError) return error
    return new RpcError(
      kind === 'image' ? 'nano/attachments/image-invalid' : 'nano/attachments/file-failed',
      `${kind} attachment failed`,
    )
  }

  cleanup(receipt) {
    if (receipt.fd !== undefined) {
      try {
        fs.closeSync(receipt.fd)
      } catch {
        // staging fd cleanup is best effort
      }
      receipt.fd = undefined
    }
    this.removeTemp(receipt)
  }

  removeTemp(receipt) {
    if (receipt.tempPath === undefined) return
    try {
      fs.rmSync(receipt.tempPath, { force: true })
    } catch {
      // a transient staging file is not security-sensitive
    }
    receipt.tempPath = undefined
  }

  assertOpen() {
    if (this.closed) throw new RpcError('nano/closed', 'the attachment host is closed')
  }
}

/** Map one admitted part to the official Agent content block. */
function toContentBlock(part) {
  if (part.type === 'text') return { type: 'text', text: part.text }
  if (part.type === 'image') return { type: 'image', attachment: part.attachment }
  if (part.type === 'file') return { type: 'file', attachment: part.attachment }
  throw new RpcError('nano/conversation/content', `admitted content type ${String(part.type)} is not supported`)
}

function requireString(value, field, max) {
  if (typeof value !== 'string' || value === '' || value.length > max) {
    throw new RpcError('jsonrpc/invalid-params', `${field} must be a non-empty string of at most ${max} characters`)
  }
  if (!/^[A-Za-z0-9._:-]+$/.test(value)) {
    throw new RpcError('jsonrpc/invalid-params', `${field} contains unsupported characters`)
  }
  return value
}

function optionalName(value) {
  if (value === undefined) return undefined
  if (typeof value !== 'string' || value === '') {
    throw new RpcError('jsonrpc/invalid-params', 'name must be a non-empty string')
  }
  // A display name never carries local path information or control characters.
  const base = value.split(/[\\/]/).pop() ?? ''
  const clean = base.replace(/[\u0000-\u001f\u007f-\u009f]/gu, '').trim()
  if (clean === '' || clean.length > MAX_NAME_LENGTH) {
    throw new RpcError('jsonrpc/invalid-params', `name must be a non-empty name of at most ${MAX_NAME_LENGTH} characters`)
  }
  return clean
}

function optionalPositiveInteger(value, field) {
  if (value === undefined) return undefined
  if (!Number.isSafeInteger(value) || value <= 0) {
    throw new RpcError('jsonrpc/invalid-params', `${field} must be a positive integer`)
  }
  return value
}

function optionalNonNegativeInteger(value, field) {
  if (value === undefined) return undefined
  if (!Number.isSafeInteger(value) || value < 0) {
    throw new RpcError('jsonrpc/invalid-params', `${field} must be a non-negative integer`)
  }
  return value
}

/** Decode canonical base64 exactly: no whitespace, no missing padding. */
function decodeCanonicalBase64(value) {
  if (typeof value !== 'string' || value.length % 4 !== 0 || !/^(?:[A-Za-z0-9+/]{4})*(?:[A-Za-z0-9+/]{2}==|[A-Za-z0-9+/]{3}=)?$/.test(value)) {
    throw new RpcError('jsonrpc/invalid-params', 'bytes must be canonical base64')
  }
  const bytes = Buffer.from(value, 'base64')
  if (bytes.toString('base64') !== value) {
    throw new RpcError('jsonrpc/invalid-params', 'bytes must be canonical base64')
  }
  return bytes
}

function rpcIdOf(value) {
  const source = value?.source
  if (source === undefined || source === null) return undefined
  if (source.kind !== 'user' || typeof source.rpcId !== 'string' || source.rpcId === '') return undefined
  return source.rpcId
}

