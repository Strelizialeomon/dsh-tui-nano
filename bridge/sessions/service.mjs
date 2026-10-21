// Session host: the official session list, renaming the active root, the
// read-only context-occupancy projection, and the official `/compact` command.
//
// S07 owns this module. Every fact comes from the official services composed in
// the Nano profile: `ctx.sessionQuery` for the cold-safe list, the live
// `ctx.sessions` store plus `ctx.sessionProjections` for the current session,
// `ctx.sessionProjectionCache` for cold title hints, `ctx.sessionTitle` for
// renaming, and `ctx.commands` for manual compaction. It never reads or writes
// the session log directly and never invents a second measurement.
import { RpcError } from '../runtime/errors.mjs'

const CONTEXT_KEYS = Object.freeze(['contextPressure', 'tokenUsage', 'contextBreakdown'])
const CONTEXT_PUBLISH_MS = 250

/**
 * One session service per host generation, bound to the exact root session.
 */
export class SessionsService {
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
    this.blank = true
    this.lastPromptAt = undefined
    this.pressureAt = undefined
    this.contextWindowAt = undefined
    this.lastPublishedContext = undefined
    this.contextTimer = undefined
    this.compactController = undefined
    this.closed = false
    this.disposers = []
    this.foldLiveState()
    this.listen()
  }

  /** Rebuild the live activity and provenance facts from the current log once. */
  foldLiveState() {
    for (const event of this.session.snapshotEvents()) {
      this.applyLiveFacts(event)
    }
  }

  /** Update the live facts and schedule one coalesced context publication. */
  listen() {
    const disposer = this.ctx.on('session/event', (subject, event) => {
      if (this.closed || subject.id !== this.rootSessionId) return
      this.applyLiveFacts(event)
      this.scheduleContextPublish()
    })
    if (typeof disposer === 'function') this.disposers.push(disposer)
  }

  /** Stop timers and listeners; the official services stay untouched. */
  close() {
    this.closed = true
    if (this.contextTimer !== undefined) {
      clearTimeout(this.contextTimer)
      this.contextTimer = undefined
    }
    for (const disposer of this.disposers.splice(0)) {
      try {
        disposer()
      } catch {
        // listener disposal is best-effort during teardown
      }
    }
  }

  /**
   * Read every official session row without activating a cold Agent or opening
   * a cold log body. Only official facts are reported; an unavailable activity
   * time or blankness is omitted rather than inferred.
   * @returns {Promise<{sessions: object[]}>} official summaries, newest first.
   */
  async list() {
    const query = this.ctx.get('sessionQuery')
    if (query === undefined) {
      throw new RpcError('nano/sessions/unavailable', 'the official session query service is not composed')
    }
    const records = await query.listSessions()
    // Keep the official query's newest-first order; S07 does not re-sort. A
    // cold row's last activity is not known cheaply, and re-sorting by a
    // synthetic key would silently diverge from the official list.
    return { sessions: records.map((record) => this.summarize(record)) }
  }

  /** Build one summary from official header, live state, and cached title hints. */
  summarize(record) {
    const header = record.header
    const live = this.ctx.sessions.get(header.id)
    const current = header.id === this.rootSessionId
    const summary = {
      sessionId: header.id,
      title: this.titleOf(header, live),
      createdAt: header.createdAt,
      agentAvailable: false,
      running: false,
      current,
    }
    if (live !== undefined) {
      const agent = this.ctx.agents.get(header.id)
      summary.agentAvailable = agent !== undefined && agent.session === live
      summary.running = agent?.status === 'running'
      summary.formatStatus = record.formatStatus ?? 'current'
    } else if (record.formatStatus !== undefined) {
      summary.formatStatus = record.formatStatus
    }
    if (current) {
      // This host owns the root session, so its log is the live source for
      // these two facts. Cold sessions stay unknown; nothing is guessed.
      summary.blank = this.blank
      summary.updatedAt = this.lastPromptAt ?? header.createdAt
    }
    if (header.cwd !== undefined) summary.cwd = header.cwd
    if (header.origin !== undefined) summary.origin = header.origin
    if (header.parentSession !== undefined) summary.parentSessionId = header.parentSession
    return summary
  }

  /** Live title from the projection; cold title from the presentation cache only. */
  titleOf(header, live) {
    if (live !== undefined) {
      const projections = this.ctx.get('sessionProjections')
      if (projections !== undefined) {
        try {
          const values = projections.snapshot(live, ['title']).values
          // The live fold is authoritative: a null title means this session
          // has no title, even if an older cache hint exists.
          return typeof values.title === 'string' ? values.title : null
        } catch {
          // A missing title unit is not a reason to fail the list.
        }
      }
    }
    const cache = this.ctx.get('sessionProjectionCache')
    if (cache === undefined) return null
    try {
      const current = cache.cachedSnapshot(header, ['title'])?.values?.title
      if (typeof current === 'string') return current
      const predecessor = cache.cachedPredecessorTitle(header)?.values?.title
      if (typeof predecessor === 'string') return predecessor
    } catch {
      // Cache hints are optional; the row simply has no title yet.
    }
    return null
  }

  /**
   * Rename the active root session through the official title service.
   * A cold session is refused: the official service would resume it and take
   * the write lock, which is exactly the second main session S07 forbids.
   * @param {object} params - `sessionId` and `title`.
   * @returns {{title: string, seq: number}} accepted title and event position.
   */
  rename(params) {
    if (typeof params?.sessionId !== 'string' || params.sessionId === '') {
      throw new RpcError('jsonrpc/invalid-params', 'sessionId must be a non-empty string')
    }
    if (typeof params.title !== 'string') {
      throw new RpcError('jsonrpc/invalid-params', 'title must be a string')
    }
    const live = this.ctx.sessions.get(params.sessionId)
    if (params.sessionId !== this.rootSessionId || live !== this.session) {
      throw new RpcError(
        'nano/sessions/not-active',
        'only the active root session can be renamed; open the session first',
      )
    }
    const titles = this.ctx.get('sessionTitle')
    if (titles === undefined) {
      throw new RpcError('nano/sessions/unavailable', 'the official session title service is not composed')
    }
    try {
      const snapshot = titles.rename(this.session, params.title)
      return { title: snapshot.title, seq: snapshot.eventSeq }
    } catch (error) {
      if (error?.name === 'SessionTitleInvalidError') {
        throw new RpcError('nano/sessions/title-invalid', 'the title is empty after normalization')
      }
      throw new RpcError(
        'nano/sessions/rejected',
        `the official title service rejected the rename: ${describe(error)}`,
      )
    }
  }

  /**
   * Read the reference context-occupancy projection of the active root.
   * The occupancy numerator and denominator stay separate raw values; the
   * client decides "unknown" when either is absent. Cumulative billing usage,
   * the last provider pressure sample, and the heuristic composition are
   * distinct facts, never merged into one "exact usage" number.
   * @returns {object} contract `sessionsContext` value.
   */
  context() {
    const projections = this.ctx.get('sessionProjections')
    if (projections === undefined) {
      throw new RpcError('nano/sessions/unavailable', 'the official session projection service is not composed')
    }
    let snapshot
    try {
      snapshot = projections.snapshot(this.session, CONTEXT_KEYS)
    } catch (error) {
      throw new RpcError('nano/sessions/rejected', `the context projection failed: ${describe(error)}`)
    }
    const pressure = snapshot.values.contextPressure ?? {}
    const usage = snapshot.values.tokenUsage
    const breakdown = snapshot.values.contextBreakdown
    const payload = {
      sessionId: this.rootSessionId,
      asOfSeq: snapshot.asOfSeq,
      approximate: true,
    }
    // A missing projection is unknown, not a zero. Do not invent billing
    // numbers when the token meter is not composed.
    if (usage !== undefined) {
      payload.tokenUsage = {
        uncachedInputTokens: usage.uncachedInputTokens ?? 0,
        outputTokens: usage.outputTokens ?? 0,
        cacheReadTokens: usage.cacheReadTokens ?? 0,
        cacheWriteTokens: usage.cacheWriteTokens ?? 0,
      }
    }
    if (Number.isInteger(pressure.projectedTokens)) payload.projectedTokens = pressure.projectedTokens
    if (Number.isInteger(pressure.contextWindow)) payload.contextWindow = pressure.contextWindow
    if (Number.isInteger(pressure.pressureTokens)) payload.pressureTokens = pressure.pressureTokens
    if (this.pressureAt !== undefined) payload.pressureAt = this.pressureAt
    if (this.contextWindowAt !== undefined) payload.contextWindowAt = this.contextWindowAt
    if (breakdown !== undefined) {
      payload.contextBreakdown = {
        systemTokens: breakdown.systemTokens ?? 0,
        toolsTokens: breakdown.toolsTokens ?? 0,
        messageTokens: breakdown.messageTokens ?? 0,
      }
    }
    return payload
  }

  /**
   * Run the official `/compact` command. The command owns every expected
   * failure message; the host passes it through verbatim instead of retrying
   * or reinterpreting it. Prompts submitted while it runs stay in the official
   * inbox and start after the compaction checkpoint.
   * @param {object} params - optional raw `input` after the command name.
   * @param {AbortSignal} [signal] - caller cancellation; the command settles as
   *   the official `Compaction cancelled.` text.
   * @returns {Promise<object>} contract `sessionsCompact` value.
   */
  async compact(params, signal) {
    if (params?.input !== undefined && typeof params.input !== 'string') {
      throw new RpcError('jsonrpc/invalid-params', 'input must be a string when present')
    }
    const commands = this.ctx.get('commands')
    if (commands === undefined) {
      throw new RpcError(
        'nano/sessions/compact-unavailable',
        'no command adapter is composed; only automatic compaction is available',
      )
    }
    if (commands.find(this.agent, 'compact') === undefined) {
      throw new RpcError(
        'nano/sessions/compact-unavailable',
        'the official /compact command is not registered in this profile',
      )
    }
    const line = params?.input ? `/compact ${params.input}` : '/compact'
    const controller = new AbortController()
    this.compactController = controller
    if (signal !== undefined) {
      if (signal.aborted) controller.abort()
      else signal.addEventListener('abort', () => controller.abort(), { once: true })
    }
    let execution
    try {
      execution = await commands.execute(this.agent, line, [], controller.signal)
    } catch (error) {
      if (controller.signal.aborted) {
        // The official command registry races the handler with the abort
        // signal (`withAbort`) and rejects before the handler can return its
        // official `Compaction cancelled.` text. Preserve that stable text and
        // let the caller reread history; the backend still finishes cleanup.
        return { kind: 'error', text: 'Compaction cancelled.' }
      }
      throw new RpcError(
        'nano/sessions/rejected',
        `the compact command failed before it settled: ${describe(error)}`,
      )
    } finally {
      if (this.compactController === controller) this.compactController = undefined
    }
    if (execution === undefined) {
      throw new RpcError('nano/sessions/rejected', 'the compact command was not resolved')
    }
    if (execution.result?.kind !== 'success' && execution.result?.kind !== 'error') {
      throw new RpcError('nano/sessions/rejected', 'the compact command returned an unknown outcome')
    }
    const result = {
      commandId: String(execution.commandId),
      kind: execution.result.kind,
    }
    if (typeof execution.result.text === 'string') result.text = execution.result.text
    if (Number.isInteger(execution.result.sourceEventSeq)) {
      result.sourceEventSeq = execution.result.sourceEventSeq
    }
    this.scheduleContextPublish()
    return result
  }

  /**
   * Stop waiting for the running manual compaction. The backend still finishes
   * its required cleanup and appends `compaction/end`; the settled command text
   * is the official `Compaction cancelled.`.
   * @returns {{cancelled: boolean}} whether a wait was in flight.
   */
  cancelCompact() {
    if (this.compactController === undefined) return { cancelled: false }
    this.compactController.abort()
    return { cancelled: true }
  }

  /** Record one durable event's activity, blankness, and projection-sample time. */
  applyLiveFacts(event) {
    if (event.type === 'turn/start') this.blank = false
    if (event.type === 'user/message' && event.data?.source?.kind === 'user') {
      if (Number.isInteger(event.time)) this.lastPromptAt = event.time
    }
    if (event.type === 'request/context' && Number.isInteger(event.time)) {
      this.contextWindowAt = event.time
    }
    if (hasUsageSample(event) && Number.isInteger(event.time)) {
      this.pressureAt = event.time
    }
  }

  /** Coalesce high-frequency projection changes into one trailing publication. */
  scheduleContextPublish() {
    if (this.closed || this.contextTimer !== undefined) return
    this.contextTimer = setTimeout(() => {
      this.contextTimer = undefined
      if (this.closed) return
      let payload
      try {
        payload = this.context()
      } catch {
        return
      }
      const encoded = JSON.stringify(payload)
      if (encoded === this.lastPublishedContext) return
      this.lastPublishedContext = encoded
      this.hub.publish('sessions.context', payload)
    }, CONTEXT_PUBLISH_MS)
    this.contextTimer.unref?.()
  }
}

/** Whether one durable Assistant settlement carries a provider usage sample. */
function hasUsageSample(event) {
  if (event.type === 'assistant/message') return event.data?.usage !== undefined
  if (event.type !== 'assistant/attempt') return false
  const stream = event.data?.stream
  if (!Array.isArray(stream)) return false
  for (let index = stream.length - 1; index >= 0; index -= 1) {
    const record = stream[index]
    if (record?.type === 'chunk' && record.chunk?.type === 'usage') return true
  }
  return false
}

function describe(error) {
  return error instanceof Error ? error.message : String(error)
}
