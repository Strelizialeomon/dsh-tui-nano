// Controls host: the command catalog, model catalog and selection, and the
// permission preset read/write path for the root session.
//
// Every fact here is read from an official service. The profile used by this
// CLI does not compose the Web `sessionController`, so the model path follows
// the official CLI adapter's approach: validate through `ctx.llm`, install the
// same request/assembly hooks `installModelSelection` installs, append the
// durable `model/selection` event, and let `ctx.agentDefaultModel` save the
// default in the background. When a deployment does compose `sessionController`
// its `selectModel`/`modelCatalog` are used verbatim.
//
// A permission switch is never `permissionPresets.set()`: it is the official
// `/permission <preset>` command, which appends the durable identity and
// injects the changed-by-user notice.
import { randomUUID } from 'node:crypto'
import { RpcError } from '../runtime/errors.mjs'

/** Durable session events that advance the model-selection fold. */
const MODEL_EVENTS = new Set(['model/selection', 'request/header'])
/** Durable session events that advance the permissions projection. */
const PERMISSION_EVENTS = new Set(['permission/preset', 'sandbox/mode', 'approval/policy'])
/** Bound copied from the official `boundContextSummary`. */
const CONTEXT_SUMMARY_MAX_CHARS = 120

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
      current: this.modelFold.pending ?? undefined,
      assembled: undefined,
    }
    this.disposers.push(...this.installSelectionHooks())
    this.listen()
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

  /**
   * Install the same routing hooks as the official `installModelSelection`:
   * the selected route overrides prompt variables and the next request, and a
   * changed route inserts the official model-change notice at the next step.
   * @returns {Array<() => void>} disposers.
   */
  installSelectionHooks() {
    const disposers = []
    const own = (disposer) => {
      if (typeof disposer === 'function') disposers.push(disposer)
    }
    const agentCtx = this.agent.ctx
    if (agentCtx === undefined || typeof agentCtx.on !== 'function') return disposers
    let assemblyOperation = 0
    own(agentCtx.on('system-prompt/assemble', async (_assembly, _context, next) => {
      const operation = ++assemblyOperation
      const selected = this.selection.current
      const assembled = await next()
      if (operation === assemblyOperation) this.selection.assembled = selected
      if (selected === undefined) return assembled
      return {
        ...assembled,
        variables: {
          ...assembled.variables,
          provider: selected.provider,
          model: selected.model,
        },
      }
    }))
    own(agentCtx.on('agent/request', async (_payload, next) => {
      const resolved = await next()
      const selected = this.selection.assembled
      if (selected === undefined) return resolved
      const { reasoningEffort: _inheritedEffort, ...withoutInheritedEffort } = resolved
      return {
        ...withoutInheritedEffort,
        provider: selected.provider,
        model: selected.model,
        ...(selected.reasoningEffort === undefined
          ? {}
          : { reasoningEffort: selected.reasoningEffort }),
      }
    }))
    own(agentCtx.on('agent/pre-step', async ({ agent, messages, signal, step }, next) => {
      const decision = await next()
      if (decision.kind === 'reject' || signal.aborted) return decision
      if (decision.messages.length === 0 && (step === 1 || messages.length > 0)) return decision
      const selected = this.selection.assembled
      const previous = agent.session.requestHeader()?.config
      if (selected === undefined || previous === undefined) return decision
      if (selected.provider === previous.provider && selected.model === previous.model) {
        return decision
      }
      const from = selected.provider === previous.provider
        ? previous.model
        : `${previous.provider}/${previous.model}`
      const to = selected.provider === previous.provider
        ? selected.model
        : `${selected.provider}/${selected.model}`
      return {
        ...decision,
        messages: [...decision.messages, modelChangeNotice(from, to)],
      }
    }, { prepend: true }))
    return disposers
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
   * command descriptors for this Agent.
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
      return await buildModelCatalog(llm, defaults.currentSelection())
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
   * carries the re-read official preset even when the command reports an error,
   * because a failed path may have written a preset identity before its knob
   * write failed.
   * @param {object} params - `preset`.
   * @returns {Promise<object>} command execution plus the actual current preset.
   */
  async setPermission(params) {
    const preset = requireNonEmpty(params?.preset, 'preset')
    const execution = await this.execute(`/permission ${preset}`)
    return { ...execution, permission: this.permissionState() }
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
      throw mapOfficialError(error, 'nano/controls/permission-rejected')
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

  /** Current permission preset plus the process-level catalog. */
  permissionState() {
    const presets = this.ctx.get('permissionPresets')
    if (presets === undefined) {
      return {
        current: null,
        catalog: { options: [], defaultOptions: [], defaultPreset: '' },
      }
    }
    let current = null
    try {
      current = presets.current(this.session)
    } catch {
      // A session whose projection is not readable yet has no current fact.
      current = null
    }
    let catalog
    try {
      catalog = withPresetConsequences(presets)
    } catch {
      catalog = { options: [], defaultOptions: [], defaultPreset: '' }
    }
    return { current, catalog }
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
 * The official same-step notice for a route change. Text and source shape are
 * copied from `installModelSelection`; S04 never invents its own wording.
 */
function modelChangeNotice(from, to) {
  const summary = `${from} → ${to}`
  return {
    id: randomUUID(),
    role: 'user',
    content: [
      {
        type: 'text',
        text: `[model changed: assistant turns above this point were generated by ${from}; the session continues with ${to}]`,
      },
    ],
    source: {
      kind: 'model-selection',
      form: 'notice',
      summary:
        summary.length <= CONTEXT_SUMMARY_MAX_CHARS
          ? summary
          : `${summary.slice(0, CONTEXT_SUMMARY_MAX_CHARS - 1)}…`,
    },
  }
}

/** Mirror of the official `buildModelCatalog` for profiles without the controller. */
async function buildModelCatalog(llm, defaultSelection) {
  const providers = llm.listProviders()
  const catalog = await Promise.all(
    providers.map(async (provider) => {
      try {
        const models = await llm.listModels(provider.id)
        const entries = await Promise.all(
          models.map(async (model) => {
            const resolved = await llm.resolveModelInfo(provider.id, model.id)
            const reasoning =
              resolved.reasoning === undefined
                ? undefined
                : {
                    efforts: resolved.reasoning.efforts.map((effort) => ({
                      id: effort.id,
                      name: effort.name,
                      ...(effort.description === undefined ? {} : { description: effort.description }),
                    })),
                    ...(resolved.reasoning.defaultEffort === undefined
                      ? {}
                      : { defaultEffort: resolved.reasoning.defaultEffort }),
                  }
            return {
              id: model.id,
              name: model.name,
              ...(model.description === undefined ? {} : { description: model.description }),
              ...(reasoning === undefined ? {} : { reasoning }),
            }
          }),
        )
        return {
          kind: 'group',
          group: { id: provider.id, name: provider.name, models: entries },
        }
      } catch (error) {
        return {
          kind: 'failure',
          failure: {
            id: provider.id,
            name: provider.name,
            message: error instanceof Error ? error.message : String(error),
          },
        }
      }
    }),
  )
  const groups = catalog
    .flatMap((item) => (item.kind === 'group' ? [item.group] : []))
    .filter((group) => group.models.length > 0)
  return {
    default: { ...defaultSelection },
    routableProviders: groups.map((group) => group.id),
    groups,
    failures: catalog.flatMap((item) => (item.kind === 'failure' ? [item.failure] : [])),
  }
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
  const message = error instanceof Error ? error.message : String(error)
  const code = typeof error?.code === 'string' ? error.code : ''
  if (code.includes('writer-held')) {
    return new RpcError('nano/controls/writer-held', message)
  }
  if (code.includes('model-unavailable')) {
    return new RpcError('nano/controls/model-unavailable', message)
  }
  return new RpcError(fallbackName, message)
}
