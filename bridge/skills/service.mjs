// Skill catalog, preview, and workspace-instruction projection.
//
// S05 owns this module. Discovery, provider invalidation, command
// adjudication, and instruction loading all stay with the official services:
// `ctx.skills.snapshot()` is the only catalog source, `skills/change` is the
// only refresh signal, and the instruction projection is folded from the
// official durable `agent-instructions` user messages. Nothing here scans a
// skill directory or parses an instruction file on its own.
import fs from 'node:fs/promises'
import { LIMITS } from '../../protocol/index.mjs'
import { RpcError } from '../runtime/errors.mjs'

/** Maximum instruction sources kept in one projection. */
export const MAX_INSTRUCTION_SOURCES = 512

/** Maximum code points of a budget notice kept in one projection. */
export const MAX_BUDGET_NOTICE_CHARS = 8192

/** Bound one official catalog observation so a hung provider cannot block refreshes forever. */
export const SKILL_DISCOVERY_TIMEOUT_MS = 20_000

/**
 * One skills service per host generation, bound to the root Agent and cwd.
 */
export class SkillsService {
  /**
   * @param {{
   *   ctx: object,
   *   hub: import('../runtime/events.mjs').SubscriptionHub,
   *   agent: object,
   *   session: object,
   *   rootSessionId: string,
   *   cwd: string,
   * }} options - host wiring.
   */
  constructor({ ctx, hub, agent, session, rootSessionId, cwd }) {
    this.ctx = ctx
    this.hub = hub
    this.agent = agent
    this.session = session
    this.rootSessionId = rootSessionId
    this.cwd = cwd
    this.address = { rootSessionId }
    this.disposers = []
    this.closed = false
    this.instructions = {
      available: false,
      baseline: false,
      loadedAt: undefined,
      sources: new Map(),
      budgetNotice: undefined,
      truncated: false,
    }
    this.indexDurableInstructions()
    this.listen()
  }

  /** Rebuild the instruction projection from the durable log of a resumed session. */
  indexDurableInstructions() {
    for (const event of this.session.snapshotEvents()) {
      if (event.type !== 'user/message') continue
      this.observeInstructionMessage(event.data, event.time)
    }
  }

  /** Subscribe to provider invalidation and instruction-bearing session messages. */
  listen() {
    const on = (event, handler) => {
      const disposer = this.ctx.on(event, handler)
      if (typeof disposer === 'function') this.disposers.push(disposer)
    }
    on('skills/change', () => {
      if (!this.closed) this.hub.publish('skills.changed', { address: this.address, reason: 'skills' })
    })
    on('commands/change', () => {
      if (!this.closed) this.hub.publish('skills.changed', { address: this.address, reason: 'commands' })
    })
    on('session/event', (subject, event) => {
      if (this.closed || subject?.id !== this.rootSessionId) return
      if (event?.type !== 'user/message') return
      if (!this.observeInstructionMessage(event.data, event.time)) return
      this.hub.publish('instructions.changed', this.instructionsPayload())
    })
  }

  /** Stop publishing and release every official listener. */
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
   * Read the current user-invocable catalog. The official registry owns
   * discovery, ordering, and completeness; command names are read from the
   * official command registry so the client can apply command-first
   * adjudication before a slash line becomes a prompt.
   * @param {object} params - `address`.
   * @returns {Promise<object>} skill list projection.
   */
  async list(params) {
    this.assertAddress(params?.address)
    const skills = this.ctx.get('skills')
    if (skills === undefined || typeof skills.snapshot !== 'function') {
      throw new RpcError('nano/unsupported', 'the official skill registry is not composed')
    }
    let snapshot
    try {
      snapshot = await skills.snapshot(await this.lookupOptions())
    } catch (error) {
      throw new RpcError(
        'jsonrpc/internal-error',
        `skill discovery failed: ${error instanceof Error ? error.message : String(error)}`,
      )
    }
    if (snapshot === null || typeof snapshot !== 'object') {
      throw new RpcError('jsonrpc/internal-error', 'the skill registry returned no catalog observation')
    }
    const commands = this.commandNames()
    const entries = []
    for (const summary of snapshot.skills ?? []) {
      const invocation = summary?.invocation
      if (invocation !== undefined && invocation !== null && invocation.userInvocable === false) continue
      if (typeof summary?.name !== 'string' || summary.name === '') continue
      const entry = {
        name: summary.name,
        description: typeof summary.description === 'string' ? summary.description : '',
        modelInvocable: !(invocation !== undefined && invocation !== null && invocation.modelInvocable === false),
        userInvocable: true,
        source: typeof summary.source === 'string' ? summary.source : 'unknown',
        provider: typeof summary.provider === 'string' ? summary.provider : 'unknown',
        shadowedByCommand: commands.includes(summary.name),
      }
      if (typeof summary.whenToUse === 'string') entry.whenToUse = summary.whenToUse
      if (typeof summary.path === 'string' && summary.path !== '') entry.path = summary.path
      entries.push(entry)
    }
    return {
      address: this.address,
      complete: snapshot.complete === true,
      commands,
      skills: entries,
    }
  }

  /**
   * Read one skill's instruction file without activating the Agent or loading
   * a body through the registry. Skills without an official path have no
   * preview; an unreadable path is an error, never an empty success.
   * @param {object} params - `address`, `name`.
   * @returns {Promise<object>} bounded preview.
   */
  async preview(params) {
    this.assertAddress(params?.address)
    const name = params?.name
    if (typeof name !== 'string' || name === '') {
      throw new RpcError('jsonrpc/invalid-params', 'name must be a non-empty string')
    }
    const skills = this.ctx.get('skills')
    if (skills === undefined || typeof skills.snapshot !== 'function') {
      throw new RpcError('nano/unsupported', 'the official skill registry is not composed')
    }
    let snapshot
    try {
      snapshot = await skills.snapshot(await this.lookupOptions())
    } catch (error) {
      throw new RpcError(
        'jsonrpc/internal-error',
        `skill discovery failed: ${error instanceof Error ? error.message : String(error)}`,
      )
    }
    if (snapshot === null || typeof snapshot !== 'object') {
      throw new RpcError('jsonrpc/internal-error', 'the skill registry returned no catalog observation')
    }
    const summary = (snapshot.skills ?? []).find(
      (candidate) => candidate?.name === name && candidate?.invocation?.userInvocable !== false,
    )
    if (summary === undefined) {
      throw new RpcError('nano/skills/not-found', `skill ${name} is not in the current user-invocable catalog`)
    }
    if (typeof summary.path !== 'string' || summary.path === '') {
      throw new RpcError('nano/skills/no-preview', `skill ${name} has no instruction file in this catalog`)
    }
    const limit = LIMITS.maxSkillPreviewBytes
    let handle
    try {
      handle = await fs.open(summary.path, 'r')
      const stat = await handle.stat()
      if (!stat.isFile()) throw new Error('not a regular file')
      const truncated = stat.size > limit
      const wanted = truncated ? limit + 3 : stat.size
      const buffer = Buffer.alloc(wanted)
      const { bytesRead } = await handle.read(buffer, 0, wanted, 0)
      const bytes = truncated ? utf8Boundary(buffer, Math.min(bytesRead, limit)) : bytesRead
      return {
        address: this.address,
        name,
        path: summary.path,
        text: buffer.subarray(0, bytes).toString('utf8'),
        truncated,
        bytes,
        limit,
      }
    } catch (error) {
      throw new RpcError(
        'nano/skills/preview-failed',
        `could not read the instruction file for ${name}: ${error instanceof Error ? error.message : String(error)}`,
      )
    } finally {
      if (handle !== undefined) await handle.close().catch(() => {})
    }
  }

  /**
   * Return the folded instruction-source projection. This is the absolute
   * baseline after a reconnect; live updates arrive as `instructions.changed`.
   * @param {object} params - `address`.
   * @returns {object} instruction projection.
   */
  snapshot(params) {
    this.assertAddress(params?.address)
    return this.instructionsPayload()
  }

  /** Serialize the folded projection in official load order. */
  instructionsPayload() {
    const payload = {
      address: this.address,
      available: this.instructions.available,
      sources: [...this.instructions.sources.values()].map((source) => ({
        path: source.path,
        action: source.action,
      })),
    }
    if (this.instructions.baseline) payload.baseline = true
    if (this.instructions.loadedAt !== undefined) payload.loadedAt = this.instructions.loadedAt
    if (this.instructions.budgetNotice !== undefined) payload.budgetNotice = this.instructions.budgetNotice
    if (this.instructions.truncated) payload.truncated = true
    return payload
  }

  /**
   * Fold one official `user/message`. Only `agent-instructions` sources are
   * observed; every other message is ignored without touching the projection.
   * @param {object} message - official UserMessage.
   * @param {number} time - durable event time in epoch milliseconds.
   * @returns {boolean} whether the projection changed.
   */
  observeInstructionMessage(message, time) {
    const source = message?.source
    if (source === undefined || source === null || source.kind !== 'agent-instructions') return false
    const baseline = source.baseline === true
    if (baseline) {
      this.instructions.sources.clear()
      this.instructions.truncated = false
    }
    for (const change of Array.isArray(source.changes) ? source.changes : []) {
      if (change === null || typeof change !== 'object') continue
      if (typeof change.path !== 'string' || change.path === '') continue
      const action = change.action === 'remove' ? 'remove' : change.action === 'replace' ? 'replace' : 'set'
      if (action === 'remove') this.instructions.sources.delete(change.path)
      else this.instructions.sources.set(change.path, { path: change.path, action })
    }
    const notice = budgetNoticeOf(textOf(message.content))
    if (notice !== undefined) this.instructions.budgetNotice = notice
    else if (baseline) this.instructions.budgetNotice = undefined
    this.instructions.available = true
    this.instructions.baseline = baseline
    if (Number.isFinite(time)) this.instructions.loadedAt = time
    this.enforceBounds()
    return true
  }

  /** Keep the projection inside the event-frame budget. */
  enforceBounds() {
    if (this.instructions.sources.size > MAX_INSTRUCTION_SOURCES) {
      const overflow = this.instructions.sources.size - MAX_INSTRUCTION_SOURCES
      let index = 0
      for (const path of this.instructions.sources.keys()) {
        if (index >= overflow) break
        this.instructions.sources.delete(path)
        index += 1
      }
      this.instructions.truncated = true
    }
    if (
      this.instructions.budgetNotice !== undefined &&
      Array.from(this.instructions.budgetNotice).length > MAX_BUDGET_NOTICE_CHARS
    ) {
      this.instructions.budgetNotice = Array.from(this.instructions.budgetNotice)
        .slice(0, MAX_BUDGET_NOTICE_CHARS)
        .join('')
      this.instructions.truncated = true
    }
  }

  /**
   * Official lookup options: the Agent is the viewing scope and the official
   * working-directory service supplies cwd, matching the model-side catalog.
   * The timeout bounds one observation; the Rust request has its own bound.
   */
  async lookupOptions() {
    const options = { scope: this.agent, signal: AbortSignal.timeout(SKILL_DISCOVERY_TIMEOUT_MS) }
    const working = this.ctx.get('workingDirectory')
    if (working !== undefined && typeof working.ensure === 'function') {
      try {
        options.cwd = await working.ensure(this.agent)
      } catch (error) {
        throw new RpcError(
          'jsonrpc/internal-error',
          `could not resolve the official working directory: ${error instanceof Error ? error.message : String(error)}`,
        )
      }
    } else {
      options.cwd = this.cwd
    }
    return options
  }

  /** Command names visible to this Agent, for command-first adjudication. */
  commandNames() {
    const commands = this.ctx.get('commands')
    if (commands === undefined || typeof commands.list !== 'function') return []
    try {
      const names = []
      for (const descriptor of commands.list(this.agent) ?? []) {
        if (typeof descriptor?.name === 'string' && descriptor.name !== '' && !names.includes(descriptor.name)) {
          names.push(descriptor.name)
        }
      }
      return names
    } catch {
      // A missing command view must not hide the skill catalog; without a
      // command name the client simply cannot shadow anything.
      return []
    }
  }

  /** Only the exact root address is served in S05. */
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
 * Extract the official budget notice from a rendered instruction message.
 * The loader renders one line naming omitted and truncated paths; this is the
 * only place the notice appears on the wire.
 * @param {string} text - model-facing instruction text.
 * @returns {string|undefined} the notice line.
 */
export function budgetNoticeOf(text) {
  const marker = 'Workspace instruction budget '
  // The official renderer puts the marker on its own first content line,
  // immediately after `<system-reminder>`. A project rule may contain the same
  // prose later; that must not masquerade as the loader's budget notice.
  const open = text.indexOf('<system-reminder>')
  if (open < 0) return undefined
  const lineStart = text.indexOf('\n', open)
  if (lineStart < 0) return undefined
  const start = lineStart + 1
  const end = text.indexOf('\n', start)
  const line = (end < 0 ? text.slice(start) : text.slice(start, end)).trim()
  return line.startsWith(marker) ? line : undefined
}

/** Concatenate text blocks exactly as rendered. */
function textOf(content) {
  if (!Array.isArray(content)) return ''
  return content
    .map((block) => (block?.type === 'text' && typeof block.text === 'string' ? block.text : ''))
    .join('')
}

/**
 * Find a UTF-8 boundary at or before `limit` so a truncated preview never
 * ends in half of a multi-byte code point.
 * @param {Buffer} buffer - bytes read from the file.
 * @param {number} limit - maximum included bytes.
 * @returns {number} inclusive byte boundary.
 */
export function utf8Boundary(buffer, limit) {
  let end = Math.min(limit, buffer.length)
  if (end === buffer.length) return end
  let continuation = 0
  let cut = end
  while (cut > 0 && continuation < 4 && (buffer[cut - 1] & 0xc0) === 0x80) {
    cut -= 1
    continuation += 1
  }
  if (cut === 0) return 0
  const lead = buffer[cut - 1]
  let needed = 1
  if ((lead & 0x80) === 0) needed = 1
  else if ((lead & 0xe0) === 0xc0) needed = 2
  else if ((lead & 0xf0) === 0xe0) needed = 3
  else if ((lead & 0xf8) === 0xf0) needed = 4
  else needed = 1
  if (cut - 1 + needed > end) end = cut - 1
  return end
}
