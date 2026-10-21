// Controls host: the command catalog, model catalog and selection, and the
// permission preset read/write path for the root session.
//
// Every fact here is read from an official service. The profile used by this
// CLI does not compose the Web `sessionController`, so the model path follows
// the official primitives: validate through `ctx.llm`, install the official
// `installModelSelection` hooks, append the durable `model/selection` event,
// and let `ctx.agentDefaultModel` save the default in the background. When a
// deployment does compose `sessionController`, its `selectModel` and
// `modelCatalog` are used verbatim.
//
// A permission switch is never `permissionPresets.set()`: it is the official
// `/permission <preset>` command, which appends the durable identity and
// injects the changed-by-user notice.
import { installModelSelection } from '@deepseek-ai/dsh-agent'
import { buildModelCatalog as buildOfficialModelCatalog } from '@deepseek-ai/dsh-api-session-controller'
import { RpcError } from '../runtime/errors.mjs'

/** Durable session events that advance the model-selection fold. */
const MODEL_EVENTS = new Set(['model/selection', 'request/header'])
/** Durable session events that advance the permissions projection. */
const PERMISSION_EVENTS = new Set(['permission/preset', 'sandbox/mode', 'approval/policy'])

/**
 * One controls service per host generation, bound to the root Agent.
 */
export class ControlsService {
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
    this.disposers = []
    this.closed = false
    this.modelFold = initialModelFold(session)
    this.selection = {
      current: this.initialSelection(),
      assembled: undefined,
    }
    this.installSelectionHooks()
    this.listen()
  }

  /**
   * The route the next request would use before any user selection: the
   * durable pending selection, then the last recorded request header, then the
   * official deployment default. The official session controller uses the same
   * order; leaving it undefined makes a cold-start request fail on an empty
   * provider/model seed.
   */
  initialSelection() {
    if (this.modelFold.pending !== null && this.modelFold.pending !== undefined) {
      return this.modelFold.pending
    }
    const header = this.session.requestHeader?.()
    const fromHeader = selectionOfConfig(header?.config, header?.adapterDefaults)
    if (fromHeader !== null) return fromHeader
    const defaults = this.ctx.get('agentDefaultModel')
    try {
      const selection = defaults?.currentSelection?.()
      if (
        typeof selection?.provider === 'string' &&
        selection.provider !== '' &&
        typeof selection?.model === 'string' &&
        selection.model !== ''
      ) {
        return {
          provider: selection.provider,
          model: selection.model,
          ...(selection.reasoningEffort === undefined
            ? {}
            : { reasoningEffort: String(selection.reasoningEffort) }),
        }
      }
    } catch {
      // A deployment without a readable default keeps the request's own route.
    }
    return undefined
  }

  /**
   * Install the official selection hooks when the Web session controller is
   * absent. The controller installs its own hooks and owns the same selection
   * ref, so installing both would duplicate routing and notices.
   */
  installSelectionHooks() {
    if (this.ctx.get('sessionController') !== undefined) return
    if (this.agent.ctx === undefined || typeof this.agent.ctx.on !== 'function') return
    try {
      const dispose = installModelSelection(this.agent.ctx, this.selection)
      if (typeof dispose === 'function') this.disposers.push(dispose)
    } catch (error) {
      this.ctx.logger?.warn?.(`controls: could not install model selection hooks: ${String(error)}`)
    }
  }

  /** Register listeners for the official projections and catalog invalidation. */
  listen() {
    const sessionDisposer = this.ctx.on('session/event', (subject, event) => {
      if (this.closed || subject.id !== this.rootSessionId) return
      if (MODEL_EVENTS.has(event.type)) {
        foldModelEvent(this.modelFold, event)
        this.publishModel()
      }
      if (PERMISSION_EVENTS.has(event.type)) this.publishPermission()
    })
    if (typeof sessionDisposer === 'function') this.disposers.push(sessionDisposer)
    const catalogDisposer = this.ctx.on('permission-presets/catalog-changed', () => {
      if (!this.closed) this.publishPermission()
    })
    if (typeof catalogDisposer === 'function') this.disposers.push(catalogDisposer)
  }

  /** Stop publishing; official services keep their own state. */
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
   * Read the current session facts without writing anything: the durable model
   * fold, the permission preset plus process catalog, and the effective
   * command descriptors for this Agent. A failed projection read is an error,
   * not an empty success, so the client keeps its last values.
   * @returns {object} one controls snapshot.
   */
  state() {
    return {
      address: this.address,
      model: this.modelState(),
      permission: this.permissionState(),
      commands: this.commands(),
    }
  }

  /**
   * Read the Host-generation model catalog. The official call isolates
   * per-provider failures in `failures`, so a partial catalog is still a fact.
   * @returns {Promise<object>} provider-grouped catalog.
   */
  async modelCatalog() {
    const controller = this.ctx.get('sessionController')
    if (controller !== undefined) {
      try {
        return await controller.modelCatalog()
      } catch (error) {
        throw mapOfficialError(error, 'nano/controls/unavailable')
      }
    }
    const llm = this.ctx.get('llm')
    const defaults = this.ctx.get('agentDefaultModel')
    if (llm === undefined || defaults === undefined) {
      throw new RpcError('nano/controls/unavailable', 'the official model registry is not composed')
    }
    try {
      return await buildCatalogInjected(this.ctx, defaults.currentSelection())
    } catch (error) {
      throw mapOfficialError(error, 'nano/controls/unavailable')
    }
  }

  /**
   * Select one complete provider/model/effort route for the session. The
   * returned selection is the Host's normalized value; the default-model save
   * happens in the background and is not part of this receipt.
   * @param {object} params - `provider`, `model`, `reasoningEffort`.
   * @returns {Promise<{selected: object}>} normalized selection.
   */
  async selectModel(params) {
    const provider = requireNonEmpty(params?.provider, 'provider')
    const model = requireNonEmpty(params?.model, 'model')
    const reasoningEffort = optionalNonEmpty(params?.reasoningEffort, 'reasoningEffort')
    const controller = this.ctx.get('sessionController')
    if (controller !== undefined) {
      try {
        const result = await controller.selectModel({
          sessionId: this.rootSessionId,
          provider,
          model,
          ...(reasoningEffort === undefined ? {} : { reasoningEffort }),
        })
        this.modelFold.pending = result.selected
        this.selection.current = result.selected
        return { selected: result.selected }
      } catch (error) {
        throw mapOfficialError(error, 'nano/controls/model-unavailable')
      }
    }

    const llm = this.ctx.get('llm')
    const defaults = this.ctx.get('agentDefaultModel')
    if (llm === undefined || defaults === undefined) {
      throw new RpcError('nano/controls/unavailable', 'the official model registry is not composed')
    }
    let selected
    try {
      const models = await llm.listModels(provider)
      if (!models.some((entry) => entry.id === model)) {
        throw new ModelUnavailableError(`unknown model ${provider}/${model}`)
      }
      const resolved = await llm.resolveCallConfig({
        provider,
        model,
        ...(reasoningEffort === undefined ? {} : { reasoningEffort }),
      })
      selected = {
        provider: resolved.provider,
        model: resolved.model,
        ...(resolved.reasoningEffort === undefined
          ? {}
          : { reasoningEffort: String(resolved.reasoningEffort) }),
      }
      this.session.append('model/selection', selected)
      this.selection.current = selected
      this.modelFold.pending = selected
    } catch (error) {
      throw mapOfficialError(error, 'nano/controls/model-unavailable')
    }
    // The default save is deliberately not awaited: the official receipt only
    // confirms the session-local selection, and a failed save is a warning.
    void Promise.resolve(defaults.saveSelection(selected)).catch((error) => {
      this.ctx.logger?.warn?.(
        `controls: session model changed but the default was not saved: ${String(error)}`,
      )
    })
    return { selected: { ...selected } }
  }

  /**
   * Switch the permission preset through the official command path. The result
   * carries the re-read official preset when that read succeeds; a failed read
   * is omitted so the client re-reads instead of assuming an empty state.
   * @param {object} params - `preset`.
   * @returns {Promise<object>} command execution plus the actual current preset.
   */
  async setPermission(params) {
    const preset = requireNonEmpty(params?.preset, 'preset')
    const execution = await this.execute(`/permission ${preset}`)
    let permission
    try {
      permission = this.permissionState()
    } catch {
      permission = undefined
    }
    return { ...execution, ...(permission === undefined ? {} : { permission }) }
  }

  /**
   * Execute one official command line. An unknown name is not an RPC failure:
   * the official registry reports it by returning `undefined`, which this
   * method surfaces as `executed: false`.
   * @param {object} params - `line`, the complete slash command.
   * @returns {Promise<object>} execution receipt.
   */
  async executeCommand(params) {
    const line = requireNonEmpty(params?.line, 'line')
    if (!line.startsWith('/')) {
      throw new RpcError('jsonrpc/invalid-params', 'line must start with "/"')
    }
    return this.execute(line)
  }

  /** Run one slash line through the official command runtime. */
  async execute(line) {
    const commands = this.ctx.get('commands')
    if (commands === undefined) {
      throw new RpcError('nano/controls/unavailable', 'the official command service is not composed')
    }
    let execution
    try {
      execution = await commands.execute(this.agent, line, [], new AbortController().signal)
    } catch (error) {
      const fallback = line.startsWith('/permission')
        ? 'nano/controls/permission-rejected'
        : 'nano/controls/command-failed'
      throw mapOfficialError(error, fallback)
    }
    if (execution === undefined) return { executed: false }
    return {
      executed: true,
      commandId: String(execution.commandId),
      kind: execution.result.kind,
      ...(execution.result.text === undefined ? {} : { text: execution.result.text }),
    }
  }

  /** Current model-selection state from the official durable fold. */
  modelState() {
    return {
      lastUsed: this.modelFold.lastUsed,
      next: this.modelFold.pending ?? this.modelFold.lastUsed ?? null,
    }
  }

  /**
   * Current permission preset plus the process-level catalog. A composed
   * service whose projection or catalog cannot be read is an error; an absent
   * service is an explicit `available: false`, distinct from an empty catalog.
   */
  permissionState() {
    const presets = this.ctx.get('permissionPresets')
    if (presets === undefined) {
      return {
        available: false,
        current: null,
        catalog: { options: [], defaultOptions: [], defaultPreset: '' },
      }
    }
    let current
    try {
      current = presets.current(this.session) ?? null
    } catch (error) {
      throw new RpcError(
        'nano/controls/unavailable',
        `the permission projection is unreadable: ${describe(error)}`,
      )
    }
    let catalog
    try {
      catalog = withPresetConsequences(presets)
    } catch (error) {
      throw new RpcError(
        'nano/controls/unavailable',
        `the permission catalog is unreadable: ${describe(error)}`,
      )
    }
    return { available: true, current, catalog }
  }

  /** Effective official command descriptors for this Agent. */
  commands() {
    const commands = this.ctx.get('commands')
    if (commands === undefined) return []
    let descriptors
    try {
      descriptors = commands.list(this.agent)
    } catch {
      return []
    }
    return descriptors.map((descriptor) => ({
      name: descriptor.name,
      description: descriptor.description,
      ...(descriptor.input?.hint === undefined ? {} : { inputHint: descriptor.input.hint }),
      ...(descriptor.definitionId === undefined
        ? {}
        : { definitionId: String(descriptor.definitionId) }),
    }))
  }

  /** Publish one live model-projection replacement. */
  publishModel() {
    try {
      this.hub.publish('controls.model', { address: this.address, model: this.modelState() })
    } catch (error) {
      this.ctx.logger?.warn?.(`controls: model event failed: ${String(error)}`)
    }
  }

  /** Publish one live permission replacement, including catalog invalidation. */
  publishPermission() {
    try {
      this.hub.publish('controls.permission', {
        address: this.address,
        permission: this.permissionState(),
      })
    } catch (error) {
      this.ctx.logger?.warn?.(`controls: permission event failed: ${String(error)}`)
    }
  }
}

/**
 * Call the official catalog builder with `llm` injected. Cordis only exposes a
 * service as a context property inside an injected child, and the official
 * builder reads `ctx.llm` directly. A plain fake context without `inject`
 * (unit tests) is called as-is.
 * @param {object} ctx - host or fake context.
 * @param {object} defaultSelection - deployment default passed explicitly.
 * @returns {Promise<object>} provider-grouped catalog.
 */
async function buildCatalogInjected(ctx, defaultSelection) {
  if (typeof ctx.inject !== 'function') {
    return buildOfficialModelCatalog(ctx, defaultSelection)
  }
  return await new Promise((resolve, reject) => {
    ctx.inject(['llm'], (child) => {
      buildOfficialModelCatalog(child, defaultSelection).then(resolve, reject)
    })
  })
}

/** A model-availability failure that maps to the official code. */
class ModelUnavailableError extends Error {
  /** @param {string} message - detail. */
  constructor(message) {
    super(message)
    this.code = 'session/model-unavailable'
  }
}

/** Initial durable fold from the session header and its event prefix. */
function initialModelFold(session) {
  const fold = { lastUsed: null, pending: null }
  const events = typeof session.snapshotEvents === 'function' ? session.snapshotEvents() : []
  for (const event of events) foldModelEvent(fold, event)
  const header = session.requestHeader?.()
  if (header?.config !== undefined) {
    fold.lastUsed = selectionOfConfig(header.config, header.adapterDefaults)
    if (sameSelection(fold.pending, fold.lastUsed)) fold.pending = null
  }
  return fold
}

/** Advance the durable fold by one official event. */
function foldModelEvent(fold, event) {
  if (event.type === 'model/selection') {
    const selection = selectionOfEvent(event.data)
    if (selection !== undefined && !sameSelection(fold.pending, selection)) {
      fold.pending = selection
    }
    return
  }
  if (event.type !== 'request/header') return
  const header = event.data?.header
  if (header?.config === undefined) return
  const lastUsed = selectionOfConfig(header.config, header.adapterDefaults)
  fold.lastUsed = lastUsed
  if (sameSelection(fold.pending, lastUsed)) fold.pending = null
}

/** Build a selection from a request header config, honoring adapter defaults. */
function selectionOfConfig(config, adapterDefaults) {
  if (typeof config?.provider !== 'string' || typeof config?.model !== 'string') return null
  const selection = { provider: config.provider, model: config.model }
  if (config.reasoningEffort !== undefined && adapterDefaults?.reasoningEffort !== true) {
    selection.reasoningEffort = String(config.reasoningEffort)
  }
  return selection
}

/** Build a selection from a `model/selection` payload. */
function selectionOfEvent(data) {
  if (typeof data?.provider !== 'string' || typeof data?.model !== 'string') return undefined
  return {
    provider: data.provider,
    model: data.model,
    ...(data.reasoningEffort === undefined ? {} : { reasoningEffort: String(data.reasoningEffort) }),
  }
}

/** Exact selection equality, mirroring the official projection. */
function sameSelection(left, right) {
  return (
    left === right ||
    (left !== null &&
      right !== null &&
      left !== undefined &&
      right !== undefined &&
      left.provider === right.provider &&
      left.model === right.model &&
      left.reasoningEffort === right.reasoningEffort)
  )
}

/**
 * Enrich the official catalog options with the exact sandbox/approval values
 * each preset writes, so a client can state the complete consequence. `custom`
 * and an unregistered `auto` keep their official option untouched.
 */
function withPresetConsequences(presets) {
  const catalog = presets.catalog()
  const enrich = (option) => {
    try {
      const spec = presets.resolve(option.value)
      return {
        ...option,
        ...(spec.sandbox === undefined ? {} : { sandbox: spec.sandbox }),
        ...(spec.approval === undefined ? {} : { approval: spec.approval }),
      }
    } catch {
      return option
    }
  }
  return {
    ...catalog,
    options: catalog.options.map(enrich),
    defaultOptions: catalog.defaultOptions.map(enrich),
  }
}

/** Validate one required non-empty string parameter. */
function requireNonEmpty(value, field) {
  if (typeof value !== 'string' || value === '') {
    throw new RpcError('jsonrpc/invalid-params', `${field} must be a non-empty string`)
  }
  return value
}

/** Validate one optional non-empty string parameter. */
function optionalNonEmpty(value, field) {
  if (value === undefined) return undefined
  return requireNonEmpty(value, field)
}

/**
 * Map an official error to a Nano contract code. Unknown official codes keep
 * their message but become the caller's fallback code, never a fabricated
 * success.
 */
function mapOfficialError(error, fallbackName) {
  const message = describe(error)
  const code = typeof error?.code === 'string' ? error.code : ''
  if (code.includes('writer-held')) {
    return new RpcError('nano/controls/writer-held', message)
  }
  if (code.includes('model-unavailable')) {
    return new RpcError('nano/controls/model-unavailable', message)
  }
  return new RpcError(fallbackName, message)
}

/** Human-readable error text without leaking non-Error values. */
function describe(error) {
  return error instanceof Error ? error.message : String(error)
}
