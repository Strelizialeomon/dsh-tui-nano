// Nano host orchestration: startup order, protocol dispatch, subscription
// fan-out, and the four-fact shutdown report.
//
// Startup order is fixed by S01 §2: resolve and claim before any session
// action, create/resume the exact root session, then handshake. A failure at
// any step is reported as nano.startupError before nano.hello, with cleanup of
// everything already allocated.
import fs from 'node:fs'
import path from 'node:path'
import { CONTRACT, LIMITS, RESERVED_METHODS } from '../../protocol/index.mjs'
import { StartupError, RpcError, ProtocolViolation, toRpcErrorBody } from './errors.mjs'
import { FrameSplitter, encodeFrame, parseFrameLine } from './frames.mjs'
import { openTransport } from './transport.mjs'
import { BoundedWriter } from './writer.mjs'
import { claimMount, releaseMount } from './ledger.mjs'
import { SubscriptionHub } from './events.mjs'
import { ConversationService } from '../conversation/service.mjs'
import { TerminalService } from '../terminal/service.mjs'
import { DecisionService } from '../decisions/service.mjs'
import { ScheduleBridge } from '../schedule/service.mjs'
import { acquireScheduleLease, releaseScheduleLease, ScheduleLeaseError } from '../schedule/lease.mjs'
import { installScheduleBackend, mountScheduleService, verifyScheduleStorage } from '../schedule/backend.mjs'
import { AttachmentService } from '../attachments/service.mjs'
import { SessionsService } from '../sessions/service.mjs'
import { ShellService } from '../shell/service.mjs'
import { JobsService } from '../jobs/service.mjs'
import { MonitorService } from '../monitor/service.mjs'
import { ControlsService } from '../controls/service.mjs'
import { SkillsService } from '../skills/service.mjs'
import { SubagentsService } from '../subagents/service.mjs'
import { buildCapabilities } from './capabilities.mjs'
import { IMPLEMENTED_REQUEST_METHODS } from './methods.mjs'

const BRIDGE_PACKAGE = JSON.parse(fs.readFileSync(new URL('../package.json', import.meta.url), 'utf8'))
export const BRIDGE_VERSION = BRIDGE_PACKAGE.version
const SUPPORTED_OFFICIAL_VERSION = '0.2.1-rc.2'
export const SHUTDOWN_STEP_NAMES = CONTRACT.shutdownSteps

/**
 * Requests whose underlying operation checks an AbortSignal. Conversation
 * mutations are intentionally absent: the official inbox operations do not
 * accept cancellation and must run to their own receipt.
 */
const CANCELLABLE_REQUEST_METHODS = new Set([
  'nano.subscribe',
  'nano.fileReferences.list',
  'nano.sessions.compact',
  'nano.schedule.create',
  'nano.schedule.update',
  'nano.schedule.delete',
  'nano.subagents.prompt',
])

/**
 * Mount the host for one profile boot.
 * @param {object} ctx - Cordis context carrying the official services.
 * @param {Record<string, unknown>} config - nano-bridge config from the runtime patch.
 * @returns {void} startup continues asynchronously; failures exit the process.
 */
export function startHost(ctx, config) {
  const state = {
    ctx,
    config: undefined,
    nonce: undefined,
    transport: undefined,
    writer: undefined,
    hub: undefined,
    conversation: undefined,
    terminal: undefined,
    decisions: undefined,
    scheduleBridge: undefined,
    scheduleFiber: undefined,
    scheduleLease: undefined,
    scheduleService: undefined,
    attachments: undefined,
    sessions: undefined,
    shell: undefined,
    jobs: undefined,
    monitor: undefined,
    controls: undefined,
    skills: undefined,
    subagents: undefined,
    agentHandle: undefined,
    session: undefined,
    claim: undefined,
    claimReleasable: false,
    mode: undefined,
    ready: false,
    accepting: false,
    shuttingDown: false,
    startupFailed: false,
    startupPromise: undefined,
    shutdownPromise: undefined,
    shutdownReport: undefined,
    transportClosed: false,
    inFlight: new Map(),
    requestCounter: 0,
    signalCount: 0,
  }

  state.startupPromise = runStartup(state, config).catch((error) => failStartup(state, error))
  installSignalHandlers(state)
  return state
}

/**
 * Describe where the profile configuration and the default credential come
 * from, without reading any secret value. The hello carries only source labels;
 * an unavailable service stays absent so the client can say "unreported".
 *
 * @param {object} ctx - Host context with the official profile and credential
 *   services.
 * @returns {Promise<object | undefined>} additive hello provenance, or
 *   undefined when neither source is known.
 */
async function describeProvenance(ctx) {
  const patchPath = ctx.get?.('profileContext')?.patchPath
  const credentials = ctx.get?.('credentials')
  let credential
  if (credentials?.describe !== undefined) {
    try {
      const info = await credentials.describe('DEEPSEEK_API_KEY')
      credential = {
        reference: 'DEEPSEEK_API_KEY',
        configured: info?.configured === true,
        ...(typeof info?.source === 'string' ? { source: info.source } : {}),
      }
    } catch {
      // A credential provider that cannot describe a reference must not turn
      // startup into a failure or invent a source.
    }
  }
  if (typeof patchPath !== 'string' && credential === undefined) return undefined
  return {
    ...(typeof patchPath === 'string' ? { config: { path: patchPath } } : {}),
    ...(credential === undefined ? {} : { credentials: credential }),
  }
}

async function runStartup(state, config) {
  state.nonce = typeof config?.nonce === 'string' ? config.nonce : undefined
  const ctx = state.ctx

  // Open the transport before config validation so a bad config can still be
  // reported through nano.startupError on fd 3/4.
  state.transport = openTransport({
    nonce: state.nonce,
    kind: typeof config?.transport === 'string' ? config.transport : process.env.NANO_TRANSPORT,
  })
  state.writer = new BoundedWriter({
    write: state.transport.write,
    onWriteError: (error) => onTransportWriteError(state, error),
  })
  startReadLoop(state)
  state.config = validateConfig(config)

  await ctx.get('loader')?.await()
  if (state.transportClosed) throw new StartupError('startup-failed', 'transport closed during startup')
  if (state.config.officialVersion !== SUPPORTED_OFFICIAL_VERSION) {
    throw new StartupError(
      'unsupported',
      `official dsh ${String(state.config.officialVersion)} is not in the verified matrix; expected ${SUPPORTED_OFFICIAL_VERSION}`,
    )
  }

  // Register the schedule backend before the loader settles. storage-domain
  // injects the service key for every routed backend, so deferring this to the
  // post-handshake path would leave other storage-domain consumers pending
  // past the loader's activation report. No task table is touched here.
  const scheduleRoot = path.join(
    state.config.dataRoot,
    'runtime-data',
    state.config.rootSessionId,
    'storage',
  )
  await installScheduleBackend(ctx, scheduleRoot)

  prepareDataRoots(state.config.dataRoot, state.config.rootSessionId)

  state.claim = await claimMount({
    dataRoot: state.config.dataRoot,
    pid: process.pid,
    sessionIds: [state.config.rootSessionId],
  })
  // A Cordis effect is a backstop for our own clean cleanup path. It releases
  // only after the cleanup steps verified that release is safe; a launcher
  // disposal that preempts an unverified cleanup keeps the claim for pid-based
  // recovery. Normal shutdown releases first, making this a no-op.
  try {
    ctx.effect(() => async () => {
      if (state.claim !== undefined && state.claimReleasable) {
        const claim = state.claim
        state.claim = undefined
        try {
          await releaseMount(claim)
        } catch {
          // The next claimant sweeps a dead pid; report only in normal shutdown.
        }
      }
    }, 'nano:mount-ledger')
  } catch {
    // A context without effect support is only possible in focused tests.
  }
  if (state.transportClosed) throw new StartupError('startup-failed', 'transport closed during mount claim')

  const cwd = await resolveCwd(ctx, state.config.cwd)
  const agents = ctx.get('agents')
  const sessions = ctx.get('sessions')
  if (agents === undefined || sessions === undefined) {
    throw new StartupError('startup-failed', 'the official agent or session service is not composed')
  }

  await inspectRootSession(ctx, state.config.sessionMode, state.config.rootSessionId)

  const route = agentRoute(state.config)
  try {
    state.agentHandle =
      state.config.sessionMode === 'create'
        ? await agents.create({
            sessionId: state.config.rootSessionId,
            meta: { cwd },
            ...(route === undefined ? {} : { agentOptions: route }),
          })
        : await agents.resume({
            resumeSessionId: state.config.rootSessionId,
            ...(route === undefined ? {} : { agentOptions: route }),
          })
    if (state.startupFailed) {
      await state.agentHandle.dispose().catch(() => {})
      throw new StartupError('startup-failed', 'startup was aborted')
    }
  } catch (error) {
    if (error?.name === 'SessionAlreadyOwnedError') {
      // The official persistence write lock is held by another process. There
      // is no pid in that error; the Nano ledger covers same-machine windows
      // before this point, so this is an external writer. Never downgrade to a
      // new session.
      throw new StartupError('occupied', `session/writer-held: ${describe(error)}`, { cause: error })
    }
    const detail = error?.name === undefined ? describe(error) : `${error.name}: ${describe(error)}`
    throw new StartupError('startup-failed', `could not ${state.config.sessionMode} root session: ${detail}`, {
      cause: error,
    })
  }
  if (state.transportClosed) throw new StartupError('startup-failed', 'transport closed while creating the root session')

  const session = state.agentHandle.agent.session
  state.session = session
  state.mode = state.config.sessionMode === 'create' ? 'created' : 'resumed'
  if (session.id !== state.config.rootSessionId) {
    throw new StartupError(
      'startup-failed',
      `session identity mismatch: requested ${state.config.rootSessionId}, received ${session.id}`,
    )
  }
  if (state.mode === 'resumed') {
    if (session.header?.cwd !== undefined && session.header.cwd !== cwd) {
      throw new StartupError(
        'startup-failed',
        `session was recorded in ${String(session.header.cwd)}, not ${cwd}`,
      )
    }
    if (session.header?.agentPreset !== undefined) {
      throw new StartupError(
        'startup-failed',
        `session runs under agent preset ${String(session.header.agentPreset)}, which this profile does not compose`,
      )
    }
  }

  if (state.startupFailed || state.transportClosed) {
    throw new StartupError('startup-failed', 'startup was aborted before the handshake')
  }

  // Materialize the session before the handshake. A crash after hello must
  // leave a resumable session; this is also the first official write checkpoint
  // that confirms the per-session write lock.
  try {
    const flushed = await ctx.sessions.flush(session)
    if (flushed !== true) throw new Error('session flush reported failure')
  } catch (error) {
    throw new StartupError('startup-failed', `could not materialize root session: ${describe(error)}`, {
      cause: error,
    })
  }

  state.hub = new SubscriptionHub({
    ctx,
    writer: state.writer,
    runtimeId: state.config.runtimeId,
    generation: state.config.generation,
    rootSessionId: state.config.rootSessionId,
    session,
    nonce: state.config.nonce,
    nonceRequired: state.transport.nonceRequired,
    authorizeChild: (address) => {
      if (state.subagents === undefined) {
        throw new RpcError('nano/subagents/unavailable', 'the subagent service is not composed')
      }
      return state.subagents.authorizeChild(address)
    },
  })
  state.attachments = new AttachmentService({
    ctx,
    agent: state.agentHandle.agent,
    session,
    rootSessionId: state.config.rootSessionId,
    dataRoot: state.config.dataRoot,
    config: state.config,
  })
  state.conversation = new ConversationService({
    ctx,
    hub: state.hub,
    agent: state.agentHandle.agent,
    session,
    rootSessionId: state.config.rootSessionId,
    attachments: state.attachments,
  })
  state.decisions = new DecisionService({
    ctx,
    hub: state.hub,
    agent: state.agentHandle.agent,
    session,
    rootSessionId: state.config.rootSessionId,
  })
  state.sessions = new SessionsService({
    ctx,
    hub: state.hub,
    agent: state.agentHandle.agent,
    session,
    rootSessionId: state.config.rootSessionId,
  })
  const controlsOfficial = await loadControlsOfficial()
  state.controls = new ControlsService({
    ctx,
    hub: state.hub,
    agent: state.agentHandle.agent,
    session,
    rootSessionId: state.config.rootSessionId,
    official: controlsOfficial,
  })
  state.skills = new SkillsService({
    ctx,
    hub: state.hub,
    agent: state.agentHandle.agent,
    session,
    rootSessionId: state.config.rootSessionId,
    cwd,
  })
  state.shell = new ShellService({
    ctx,
    agent: state.agentHandle.agent,
    session,
    rootSessionId: state.config.rootSessionId,
  })
  state.jobs = new JobsService({
    ctx,
    hub: state.hub,
    session,
    rootSessionId: state.config.rootSessionId,
  })
  state.monitor = new MonitorService({
    ctx,
    hub: state.hub,
    agent: state.agentHandle.agent,
    session,
    rootSessionId: state.config.rootSessionId,
  })
  state.terminal = new TerminalService({
    ctx,
    writer: state.writer,
    agent: state.agentHandle.agent,
    session,
    rootSessionId: state.config.rootSessionId,
    nonce: state.config.nonce,
    nonceRequired: state.transport.nonceRequired,
  })
  state.subagents = new SubagentsService({
    ctx,
    hub: state.hub,
    agent: state.agentHandle.agent,
    session,
    rootSessionId: state.config.rootSessionId,
    attachments: state.attachments,
  })

  // Both ownership gates (mount ledger above, schedule lease inside) must be
  // held before the official scheduler is mounted. A storage or route failure
  // ends startup with `unsupported` instead of running without a task table.
  state.scheduleFiber = await startScheduleHost(state)

  const attachmentLimits = state.attachments.helloLimits()
  const provenance = await describeProvenance(ctx)
  const hello = {
    jsonrpc: '2.0',
    method: 'nano.hello',
    params: {
      protocol: { major: CONTRACT.protocol.major, minor: CONTRACT.protocol.minor },
      runtimeId: state.config.runtimeId,
      generation: state.config.generation,
      versions: {
        official: state.config.officialVersion,
        bridge: BRIDGE_VERSION,
        node: process.version,
      },
      cwd,
      rootSessionId: state.config.rootSessionId,
      session: {
        mode: state.mode,
        seq: session.seq,
        header: session.header,
      },
      capabilities: buildCapabilities(ctx, state.config, {
        schedule: state.scheduleService !== undefined,
      }),
      limits: {
        maxFrameBytes: LIMITS.maxFrameBytes,
        maxPendingRequests: LIMITS.maxPendingRequests,
        maxAttachmentChunkBytes: LIMITS.maxAttachmentChunkBytes,
      },
      ...(attachmentLimits === undefined ? {} : { attachmentLimits }),
      ...(provenance === undefined ? {} : { provenance }),
    },
  }
  await state.writer.sendControl(
    encodeFrame(hello, { nonce: state.config.nonce, requireNonce: state.transport.nonceRequired }),
  )
  state.ready = true
  state.accepting = true
}

function validateConfig(config) {
  if (config === null || typeof config !== 'object' || Array.isArray(config)) {
    throw new StartupError('startup-failed', 'nano-bridge requires a runtime config object')
  }
  const requiredStrings = ['runtimeId', 'rootSessionId', 'cwd', 'dataRoot', 'officialVersion', 'nonce']
  for (const field of requiredStrings) {
    if (typeof config[field] !== 'string' || config[field] === '') {
      throw new StartupError('startup-failed', `nano-bridge config.${field} must be a non-empty string`)
    }
  }
  if (!Number.isInteger(config.generation) || config.generation <= 0) {
    throw new StartupError('startup-failed', 'nano-bridge config.generation must be a positive integer')
  }
  if (config.sessionMode !== 'create' && config.sessionMode !== 'resume') {
    throw new StartupError('startup-failed', 'nano-bridge config.sessionMode must be create or resume')
  }
  if (!path.isAbsolute(config.cwd)) {
    throw new StartupError('startup-failed', 'nano-bridge config.cwd must be absolute')
  }
  if (!path.isAbsolute(config.dataRoot)) {
    throw new StartupError('startup-failed', 'nano-bridge config.dataRoot must be absolute')
  }
  if ((config.provider === undefined) !== (config.model === undefined)) {
    throw new StartupError('startup-failed', 'nano-bridge config.provider and config.model must be supplied together')
  }
  for (const field of ['provider', 'model', 'reasoningEffort']) {
    if (config[field] !== undefined && (typeof config[field] !== 'string' || config[field] === '')) {
      throw new StartupError('startup-failed', `nano-bridge config.${field} must be a non-empty string when present`)
    }
  }
  return { ...config }
}

/**
 * Load the official helpers the controls host installs directly. The import
 * lives here, not in `controls/service.mjs`, so the bridge can be imported by
 * unit tests without the profile's installed official packages. A missing
 * package is a startup failure, never a silently disabled control path.
 * @returns {Promise<{installModelSelection: Function, buildModelCatalog: Function}>} official helpers.
 */
async function loadControlsOfficial() {
  try {
    const [agent, controller] = await Promise.all([
      import('@deepseek-ai/dsh-agent'),
      import('@deepseek-ai/dsh-api-session-controller'),
    ])
    if (
      typeof agent.installModelSelection !== 'function' ||
      typeof controller.buildModelCatalog !== 'function'
    ) {
      throw new Error('the installed official packages do not export the expected helpers')
    }
    return {
      installModelSelection: agent.installModelSelection,
      buildModelCatalog: controller.buildModelCatalog,
    }
  } catch (error) {
    throw new StartupError(
      'startup-failed',
      `could not load the official model-selection helpers: ${describe(error)}`,
      { cause: error },
    )
  }
}

/** Build the optional initial Agent route from the runtime patch. */
function agentRoute(config) {
  if (config.provider === undefined) return undefined
  const route = { provider: config.provider, model: config.model }
  if (config.reasoningEffort !== undefined) route.reasoningEffort = config.reasoningEffort
  return route
}

/**
 * Check root-session existence before create/resume. Resume must never invent
 * a same-id empty session, and create must never write over an existing log.
 * `stat` is the official header-only persistence read; it does not open the log
 * body or activate a cold agent.
 * @param {object} ctx - host context.
 * @param {'create'|'resume'} mode - requested lifecycle.
 * @param {string} rootSessionId - exact official identity.
 * @returns {Promise<void>} resolves when the identity is usable for the mode.
 */
async function inspectRootSession(ctx, mode, rootSessionId) {
  const persistence = ctx.get('sessionPersistence')
  if (persistence === undefined) return
  let stored
  try {
    stored = await persistence.stat(rootSessionId)
  } catch (error) {
    throw new StartupError(
      'startup-failed',
      `could not inspect root session ${rootSessionId}: ${describe(error)}`,
      { cause: error },
    )
  }
  if (mode === 'resume' && stored === undefined) {
    throw new StartupError('startup-failed', `session/not-found: no stored session ${rootSessionId}`)
  }
  if (mode === 'create' && stored !== undefined) {
    throw new StartupError(
      'startup-failed',
      `session identity ${rootSessionId} already exists; refusing to create over it`,
    )
  }
}

function prepareDataRoots(dataRoot, rootSessionId) {
  const directories = [
    dataRoot,
    path.join(dataRoot, 'sessions'),
    path.join(dataRoot, 'runtime-data', rootSessionId, 'storage'),
  ]
  try {
    for (const directory of directories) {
      fs.mkdirSync(directory, { recursive: true, mode: 0o700 })
      fs.chmodSync(directory, 0o700)
    }
  } catch (error) {
    throw new StartupError('startup-failed', `could not prepare Nano data directories under ${dataRoot}`, { cause: error })
  }
}

async function resolveCwd(ctx, configuredCwd) {
  const fsService = ctx.get('fs')
  if (fsService === undefined) return configuredCwd
  try {
    return fsService.processPath(await fsService.resolve(configuredCwd))
  } catch (error) {
    throw new StartupError('startup-failed', `could not resolve the working directory ${configuredCwd}`, { cause: error })
  }
}

/**
 * Acquire the schedule lease, route the schedule domain to the per-session
 * backend, prove the route, and mount the official scheduler. Any failure is a
 * startup failure: the host never keeps running without the table it claims to
 * own, and it never falls back to the shared `storages` root.
 * @param {object} state - host state with the root Agent and hub already live.
 * @returns {Promise<object>} the mounted schedule fiber.
 */
async function startScheduleHost(state) {
  const { dataRoot, rootSessionId } = state.config
  const root = path.join(dataRoot, 'runtime-data', rootSessionId, 'storage')
  let lease
  try {
    lease = acquireScheduleLease({ dataRoot, rootSessionId })
  } catch (error) {
    if (error instanceof ScheduleLeaseError && error.code === 'held') {
      throw new StartupError('occupied', error.message, { holderPid: error.holderPid, cause: error })
    }
    throw new StartupError('unsupported', error instanceof Error ? error.message : String(error), { cause: error })
  }
  state.scheduleLease = lease
  try {
    provideSessionController(state)
    await verifyScheduleStorage(state.ctx, { root })
    const { fiber, service } = await mountScheduleService(state.ctx)
    state.scheduleService = service
    state.scheduleBridge = new ScheduleBridge({
      ctx: state.ctx,
      hub: state.hub,
      schedule: service,
      rootSessionId,
    })
    return fiber
  } catch (error) {
    releaseScheduleLease(lease)
    state.scheduleLease = undefined
    throw error instanceof StartupError ? error : new StartupError('unsupported', describe(error), { cause: error })
  }
}

/**
 * Provide the single official-controller method the scheduler calls.
 *
 * The Nano profile composes dsh-base, not the Web session controller, so
 * `ctx.sessionController` would otherwise be absent and dsh-schedule would stay
 * pending forever. This adapter resolves only the root Agent this host already
 * owns; it creates, resumes, and configures nothing.
 * @param {object} state - host state.
 */
function provideSessionController(state) {
  if (state.ctx.get('sessionController') !== undefined) return
  state.ctx.provide('sessionController', {
    resolveAgent: async (sessionId) => {
      if (sessionId !== state.config.rootSessionId) {
        return { error: new Error(`session ${String(sessionId)} is not served by this host`) }
      }
      if (state.agentHandle === undefined) {
        return { error: new Error('the root Agent is no longer live') }
      }
      return { agent: state.agentHandle.agent }
    },
  })
}

function startReadLoop(state) {
  void (async () => {
    const splitter = new FrameSplitter()
    try {
      for await (const chunk of state.transport.input) {
        for (const line of splitter.push(chunk)) {
          handleLine(state, line)
        }
      }
      splitter.end()
      state.transportClosed = true
      if (state.ready && !state.shuttingDown) {
        void beginShutdown(state, 'transport closed', undefined, 1)
      } else if (!state.ready && !state.startupFailed) {
        void failStartup(state, new StartupError('startup-failed', 'transport closed during startup'))
      }
    } catch (error) {
      state.transportClosed = true
      if (error instanceof ProtocolViolation && isRecoverable(error)) {
        sendErrorResponse(state, null, error)
        return
      }
      if (!state.ready && !state.startupFailed) {
        void failStartup(state, toStartupFailure(error))
      } else if (!state.shuttingDown) {
        void beginShutdown(state, `protocol violation: ${describe(error)}`, undefined, 1)
      }
    }
  })()
}

function handleLine(state, line) {
  let frame
  try {
    frame = parseFrameLine(line, {
      requireNonce: state.transport.nonceRequired,
      nonce: state.config?.nonce ?? state.nonce,
    })
  } catch (error) {
    if (error instanceof ProtocolViolation && isRecoverable(error)) {
      sendErrorResponse(state, error.requestId ?? null, error)
      return
    }
    state.transportClosed = true
    if (!state.ready && !state.startupFailed) {
      void failStartup(state, toStartupFailure(error))
    } else if (!state.shuttingDown) {
      void beginShutdown(state, `protocol violation: ${describe(error)}`, undefined, 1)
    }
    return
  }

  if (frame.type === 'response') {
    // The host never sends requests in v1; an unsolicited response is a
    // protocol violation, not something to guess about.
    sendErrorResponse(state, frame.id, new ProtocolViolation('unsolicited response frame'))
    return
  }
  if (frame.type === 'notification') {
    // S09 input/resize are the only client-to-host notifications. They carry no
    // response; an invalid one ends that attachment explicitly.
    if (state.terminal?.handleNotification(frame.method, frame.params) === true) return
    // Other reserved notification methods are rejected silently.
    return
  }

  if (state.inFlight.has(frame.id)) {
    sendErrorResponse(state, frame.id, new RpcError('jsonrpc/invalid-request', 'duplicate in-flight request id'))
    return
  }
  if (state.inFlight.size >= LIMITS.maxPendingRequests) {
    sendErrorResponse(state, frame.id, new RpcError('nano/pending-limit', 'too many pending requests'))
    return
  }
  const entry = {
    method: frame.method,
    cancellable: CANCELLABLE_REQUEST_METHODS.has(frame.method),
    controller: new AbortController(),
  }
  state.inFlight.set(frame.id, entry)
  void dispatchRequest(state, frame)
    .catch((error) => {
      sendErrorResponse(state, frame.id, error)
    })
    .finally(() => {
      state.inFlight.delete(frame.id)
    })
}

async function dispatchRequest(state, frame) {
  const { id, method, params } = frame
  if (state.shuttingDown && method !== 'nano.shutdown') {
    throw new RpcError('nano/closed', 'host is shutting down')
  }
  if (!state.ready) {
    throw new RpcError('nano/not-ready', 'host is still starting')
  }
  if (!IMPLEMENTED_REQUEST_METHODS.includes(method)) {
    const reserved = RESERVED_METHODS.get(method)
    if (reserved !== undefined) {
      throw new RpcError('nano/unsupported', `${method} is reserved for ${reserved.owner} and is not implemented by this generation`)
    }
    throw new RpcError('jsonrpc/method-not-found', `unknown method ${method}`)
  }
  switch (method) {
    case 'nano.shutdown': {
      const report = await beginShutdown(state, typeof params.reason === 'string' ? params.reason : 'client request', id, 0)
      return report
    }
    case 'nano.subscribe': {
      const entry = state.inFlight.get(id)
      const result = await state.hub.subscribe(params, entry?.controller.signal)
      await sendResult(state, id, result)
      return undefined
    }
    case 'nano.unsubscribe': {
      const result = state.hub.unsubscribe(params.subscriptionId)
      await sendResult(state, id, result)
      return undefined
    }
    case 'nano.cancelRequest': {
      const target = state.inFlight.get(params.id)
      if (target === undefined || target.method === 'nano.shutdown' || target.cancellable !== true) {
        await sendResult(state, id, { cancelled: false })
        return undefined
      }
      target.controller.abort()
      await sendResult(state, id, { cancelled: true })
      return undefined
    }
    case 'nano.attachment.begin': {
      await sendResult(state, id, state.attachments.begin(params))
      return undefined
    }
    case 'nano.attachment.chunk': {
      await sendResult(state, id, state.attachments.chunk(params))
      return undefined
    }
    case 'nano.attachment.finish': {
      await sendResult(state, id, await state.attachments.finish(params))
      return undefined
    }
    case 'nano.attachment.discard': {
      await sendResult(state, id, state.attachments.discard(params))
      return undefined
    }
    case 'nano.fileReferences.list': {
      const entry = state.inFlight.get(id)
      await sendResult(
        state,
        id,
        await state.attachments.listReferences(params, entry?.controller.signal),
      )
      return undefined
    }
    case 'nano.conversation.submit': {
      await sendResult(state, id, await state.conversation.submit(params))
      return undefined
    }
    case 'nano.conversation.editQueued': {
      await sendResult(state, id, state.conversation.editQueued(params))
      return undefined
    }
    case 'nano.conversation.deleteQueued': {
      await sendResult(state, id, state.conversation.deleteQueued(params))
      return undefined
    }
    case 'nano.conversation.steerQueued': {
      await sendResult(state, id, state.conversation.steerQueued(params))
      return undefined
    }
    case 'nano.conversation.cancel': {
      await sendResult(state, id, state.conversation.cancel(params))
      return undefined
    }
    case 'nano.conversation.snapshot': {
      await sendResult(state, id, state.conversation.snapshot(params))
      return undefined
    }
    case 'nano.terminal.environment': {
      await sendResult(state, id, state.terminal.environment(signalOf(state, id)))
      return undefined
    }
    case 'nano.terminal.shells': {
      await sendResult(state, id, { shells: await state.terminal.shells(signalOf(state, id)) })
      return undefined
    }
    case 'nano.terminal.list': {
      await sendResult(state, id, state.terminal.list())
      return undefined
    }
    case 'nano.terminal.create': {
      await sendResult(state, id, await state.terminal.create(params, signalOf(state, id)))
      return undefined
    }
    case 'nano.terminal.retain': {
      await sendResult(state, id, await state.terminal.retain(params, signalOf(state, id)))
      return undefined
    }
    case 'nano.terminal.release': {
      await sendResult(state, id, state.terminal.release(params))
      return undefined
    }
    case 'nano.terminal.attach': {
      await sendResult(state, id, await state.terminal.attach(params, signalOf(state, id)))
      return undefined
    }
    case 'nano.terminal.detach': {
      await sendResult(state, id, state.terminal.detach(params))
      return undefined
    }
    case 'nano.terminal.rename': {
      await sendResult(state, id, state.terminal.rename(params))
      return undefined
    }
    case 'nano.terminal.close': {
      await sendResult(state, id, await state.terminal.close(params))
      return undefined
    }
    case 'nano.subagents.snapshot': {
      await sendResult(state, id, await state.subagents.snapshot())
      return undefined
    }
    case 'nano.subagents.list': {
      await sendResult(state, id, await state.subagents.list())
      return undefined
    }
    case 'nano.subagents.describe': {
      await sendResult(state, id, await state.subagents.describe(params))
      return undefined
    }
    case 'nano.subagents.prompt': {
      await sendResult(state, id, await state.subagents.prompt(params, signalOf(state, id)))
      return undefined
    }
    case 'nano.subagents.interrupt': {
      await sendResult(state, id, await state.subagents.interrupt(params))
      return undefined
    }
    case 'nano.approval.answer': {
      await sendResult(state, id, state.decisions.answerApproval(params))
      return undefined
    }
    case 'nano.question.answer': {
      await sendResult(state, id, state.decisions.answerQuestion(params))
      return undefined
    }
    case 'nano.decisions.snapshot': {
      await sendResult(state, id, state.decisions.snapshot(params))
      return undefined
    }
    case 'nano.schedule.catalog': {
      await sendResult(state, id, await state.scheduleBridge.catalog(params))
      return undefined
    }
    case 'nano.schedule.list': {
      await sendResult(state, id, await state.scheduleBridge.list(params))
      return undefined
    }
    case 'nano.schedule.create': {
      const signal = state.inFlight.get(id)?.controller.signal
      await sendResult(state, id, await state.scheduleBridge.create(params, signal))
      return undefined
    }
    case 'nano.schedule.update': {
      const signal = state.inFlight.get(id)?.controller.signal
      await sendResult(state, id, await state.scheduleBridge.update(params, signal))
      return undefined
    }
    case 'nano.schedule.delete': {
      const signal = state.inFlight.get(id)?.controller.signal
      await sendResult(state, id, await state.scheduleBridge.delete(params, signal))
      return undefined
    }
    case 'nano.schedule.history': {
      await sendResult(state, id, await state.scheduleBridge.history(params))
      return undefined
    }
    case 'nano.sessions.list': {
      await sendResult(state, id, await state.sessions.list())
      return undefined
    }
    case 'nano.sessions.rename': {
      await sendResult(state, id, state.sessions.rename(params))
      return undefined
    }
    case 'nano.sessions.context': {
      await sendResult(state, id, state.sessions.context())
      return undefined
    }
    case 'nano.sessions.compact': {
      const signal = state.inFlight.get(id)?.controller.signal
      await sendResult(state, id, await state.sessions.compact(params, signal))
      return undefined
    }
    case 'nano.sessions.compactCancel': {
      await sendResult(state, id, state.sessions.cancelCompact())
      return undefined
    }
    case 'nano.shell.exec': {
      await sendResult(state, id, await state.shell.exec(params))
      return undefined
    }
    case 'nano.jobs.snapshot': {
      await sendResult(state, id, state.jobs.snapshot(params))
      return undefined
    }
    case 'nano.jobs.follow': {
      await sendResult(state, id, state.jobs.follow(params))
      return undefined
    }
    case 'nano.jobs.kill': {
      await sendResult(state, id, state.jobs.kill(params))
      return undefined
    }
    case 'nano.monitor.create': {
      await sendResult(state, id, state.monitor.create(params))
      return undefined
    }
    case 'nano.monitor.list': {
      await sendResult(state, id, state.monitor.list(params))
      return undefined
    }
    case 'nano.monitor.stop': {
      await sendResult(state, id, state.monitor.stop(params))
      return undefined
    }
    case 'nano.monitor.resetBudget': {
      await sendResult(state, id, state.monitor.resetBudget(params))
      return undefined
    }
    case 'nano.controls.state': {
      await sendResult(state, id, state.controls.state())
      return undefined
    }
    case 'nano.controls.modelCatalog': {
      await sendResult(state, id, await state.controls.modelCatalog())
      return undefined
    }
    case 'nano.controls.selectModel': {
      await sendResult(state, id, await state.controls.selectModel(params))
      return undefined
    }
    case 'nano.controls.setPermission': {
      await sendResult(state, id, await state.controls.setPermission(params))
      return undefined
    }
    case 'nano.controls.executeCommand': {
      await sendResult(state, id, await state.controls.executeCommand(params))
      return undefined
    }
    case 'nano.skills.list': {
      await sendResult(state, id, await state.skills.list(params))
      return undefined
    }
    case 'nano.skills.preview': {
      await sendResult(state, id, await state.skills.preview(params))
      return undefined
    }
    case 'nano.instructions.snapshot': {
      await sendResult(state, id, state.skills.snapshot(params))
      return undefined
    }
    default: {
      const reserved = RESERVED_METHODS.get(method)
      if (reserved !== undefined) {
        throw new RpcError('nano/unsupported', `${method} is reserved for ${reserved.owner} and is not implemented by this generation`)
      }
      throw new RpcError('jsonrpc/method-not-found', `unknown method ${method}`)
    }
  }
}

function signalOf(state, id) {
  return state.inFlight.get(id)?.controller.signal
}

async function sendResult(state, id, result) {
  const frame = { jsonrpc: '2.0', id, result }
  await state.writer.sendControl(encodeFrame(frame, { nonce: state.config.nonce, requireNonce: state.transport.nonceRequired }))
}

function sendErrorResponse(state, id, error) {
  if (state.writer === undefined) return
  const frame = { jsonrpc: '2.0', id, error: toRpcErrorBody(error) }
  try {
    state.writer
      .sendControl(
        encodeFrame(frame, {
          nonce: state.config?.nonce ?? state.nonce,
          requireNonce: state.transport?.nonceRequired ?? false,
        }),
      )
      .catch(() => {})
  } catch {
    // A frame that cannot be encoded means the transport is already unusable.
  }
}

/**
 * Run the one shutdown path. Returns the report to a waiting request; exits the
 * process afterwards. A signal or transport EOF passes no request id.
 * @param {object} state - host state.
 * @param {string} reason - diagnostic reason.
 * @param {string|undefined} requestId - request awaiting the report, if any.
 * @param {number} exitCode - exit code to use when no request remains.
 * @returns {Promise<object>} shutdown report.
 */
async function beginShutdown(state, reason, requestId, exitCode) {
  if (state.shutdownPromise !== undefined) {
    const report = await state.shutdownPromise
    if (requestId !== undefined) await sendResult(state, requestId, report)
    return report
  }
  state.shuttingDown = true
  state.accepting = false
  state.shutdownPromise = runShutdown(state, reason)
  const report = await state.shutdownPromise
  state.shutdownReport = report
  if (requestId !== undefined) {
    try {
      await sendResult(state, requestId, report)
    } catch {
      // The writer already failed; the exit path still reports locally.
    }
  }
  const code = report.clean ? 0 : 1
  await finishExit(state, exitCode === 0 ? code : Math.max(code, 1))
  return report
}

async function runShutdown(state, reason) {
  const started = performance.now()
  const deadline = started + LIMITS.gracefulShutdownMs
  const steps = []
  const faults = state.config.allowFaults === true && typeof state.config.faults === 'object' ? state.config.faults : {}

  if (faults.hangShutdown === true) {
    // Test-only fault: exercise the Rust-side 10s + 3s termination budget.
    await new Promise(() => {})
  }

  steps.push({ name: 'accepting', status: 'stopped' })

  steps.push(await runStep('subscriptions', deadline, () => {
    state.controls?.close()
    state.controls = undefined
    state.decisions?.close()
    state.decisions = undefined
    state.conversation?.close()
    state.conversation = undefined
    state.attachments?.close()
    state.attachments = undefined
    state.sessions?.close()
    state.sessions = undefined
    state.skills?.close()
    state.skills = undefined
    state.subagents?.close()
    state.subagents = undefined
    state.shell?.close()
    state.shell = undefined
    state.jobs?.close()
    state.jobs = undefined
    state.monitor?.close()
    state.monitor = undefined
    state.hub?.closeAll(reason)
  }, faults))

  steps.push(await runStep('agents', deadline, async () => {
    // Stop the scheduler before the Agent it delivers to. Disposing the fiber
    // drains an accepted delivery and closes the schedule domain.
    if (state.scheduleFiber !== undefined) {
      const fiber = state.scheduleFiber
      state.scheduleFiber = undefined
      state.scheduleBridge?.close()
      state.scheduleBridge = undefined
      await fiber.dispose()
    }
    // User terminals are owned by this host; close their process range before
    // the Session owner is disposed. Terminal output is never persisted.
    let terminalError
    if (state.terminal !== undefined) {
      try {
        await state.terminal.closeAll()
      } catch (error) {
        terminalError = error
      } finally {
        state.terminal = undefined
      }
    }
    if (state.agentHandle !== undefined) {
      // Cancel the main Agent subtree without unregistering the Session yet; the
      // persistence flush below still needs a live Session in the store.
      state.agentHandle.agent.cancel({ kind: 'user' })
      await state.agentHandle.agent.whenIdle()
    }
    if (terminalError !== undefined) throw terminalError
  }, faults))

  steps.push(await runStep('persistence', deadline, async () => {
    if (faults.failPersistence === true) throw new Error('injected persistence failure')
    if (state.session === undefined) return
    const ok = await state.ctx.sessions.flush(state.session)
    if (ok !== true) throw new Error('session flush reported failure')
  }, faults))

  steps.push(await runStep('dispose', deadline, async () => {
    if (state.agentHandle === undefined) return
    await state.agentHandle.dispose()
    state.agentHandle = undefined
  }, faults))

  // A failed cleanup means the ownership facts are unverified. Keep the mount
  // record for pid-based recovery instead of releasing early; a dead pid is
  // swept by the next claimant.
  if (steps.some((step) => step.status !== 'stopped')) {
    state.claimReleasable = false
    steps.push({ name: 'ledger', status: 'failed', detail: 'claim retained because an earlier cleanup step failed' })
  } else {
    state.claimReleasable = true
    steps.push(await runStep('ledger', deadline, async () => {
      // Release the schedule lease only after the scheduler stopped and the
      // session flushed; a failed earlier step keeps both ownership facts.
      if (state.scheduleLease !== undefined) {
        releaseScheduleLease(state.scheduleLease)
        state.scheduleLease = undefined
      }
      if (state.claim === undefined) return
      await releaseMount(state.claim)
      state.claim = undefined
    }, faults))
    if (steps[steps.length - 1].status !== 'stopped') state.claimReleasable = false
  }

  const clean = steps.every((step) => step.status === 'stopped')
  return { steps, clean }
}

async function runStep(name, deadline, operation, faults = {}) {
  if (faults.unknownStep === name) {
    return { name, status: 'unknown', detail: 'injected unknown step' }
  }
  const remaining = deadline - performance.now()
  if (remaining <= 0) return { name, status: 'unknown', detail: 'shutdown budget exhausted' }
  try {
    await withTimeout(Promise.resolve().then(operation), remaining, name)
    return { name, status: 'stopped' }
  } catch (error) {
    const timeout = error instanceof StepTimeout
    return {
      name,
      status: timeout ? 'unknown' : 'failed',
      detail: truncate(describe(error), 300),
    }
  }
}

async function finishExit(state, code) {
  if (state.finished) return
  state.finished = true
  try {
    await state.writer?.flush({ timeoutMs: Math.min(2000, LIMITS.gracefulShutdownMs) })
  } catch {
    // A blocked or dead transport may prevent the final flush; the exit code
    // still reflects the cleanup result.
  }
  state.writer?.close()
  state.transport?.close()
  process.exitCode = code
  const appExit = state.ctx.get('appExit')
  if (typeof appExit === 'function') {
    // appExit returns the launcher's disposal promise. Await it so the tree is
    // shut down before the process can exit; the watchdog covers a disposal
    // that never settles without keeping the loop alive itself.
    const pending = appExit(code)
    if (pending !== undefined && typeof pending.then === 'function') {
      await Promise.race([pending, delay(LIMITS.termGraceMs + 3000)])
    }
  }
  const watchdog = setTimeout(() => {
    // process.exit() can hang joining a libuv thread-pool task. SIGKILL is the
    // only reliable final bound after our own cleanup and the protocol flush.
    try {
      process.kill(process.pid, 'SIGKILL')
    } catch {
      // already gone
    }
  }, LIMITS.termGraceMs + 3000)
  watchdog.unref?.()
}

async function failStartup(state, error) {
  if (state.startupFailed || state.shuttingDown) return
  state.startupFailed = true
  state.accepting = false
  const startupError = error instanceof StartupError ? error : toStartupFailure(error)

  // Clean up anything allocated before the failure. Persistence is flushed
  // BEFORE the Agent is disposed: dispose removes the Session from the store,
  // after which sessions.flush can never succeed.
  let code = startupError.code
  let message = startupError.message
  const cleanupFailures = []
  if (state.controls !== undefined) {
    try {
      state.controls.close()
      state.controls = undefined
    } catch (cleanupError) {
      cleanupFailures.push(`controls: ${describe(cleanupError)}`)
    }
  }
  if (state.decisions !== undefined) {
    try {
      state.decisions.close()
      state.decisions = undefined
    } catch (cleanupError) {
      cleanupFailures.push(`decisions: ${describe(cleanupError)}`)
    }
  }
  if (state.conversation !== undefined) {
    try {
      state.conversation.close()
      state.conversation = undefined
    } catch (cleanupError) {
      cleanupFailures.push(`conversation: ${describe(cleanupError)}`)
    }
  }
  if (state.attachments !== undefined) {
    try {
      state.attachments.close()
      state.attachments = undefined
    } catch (cleanupError) {
      cleanupFailures.push(`attachments: ${describe(cleanupError)}`)
    }
  }
  if (state.sessions !== undefined) {
    try {
      state.sessions.close()
      state.sessions = undefined
    } catch (cleanupError) {
      cleanupFailures.push(`sessions: ${describe(cleanupError)}`)
    }
  }
  if (state.skills !== undefined) {
    try {
      state.skills.close()
      state.skills = undefined
    } catch (cleanupError) {
      cleanupFailures.push(`skills: ${describe(cleanupError)}`)
    }
  }
  if (state.subagents !== undefined) {
    try {
      state.subagents.close()
      state.subagents = undefined
    } catch (cleanupError) {
      cleanupFailures.push(`subagents: ${describe(cleanupError)}`)
    }
  }
  if (state.shell !== undefined) {
    try {
      state.shell.close()
      state.shell = undefined
    } catch (cleanupError) {
      cleanupFailures.push(`shell: ${describe(cleanupError)}`)
    }
  }
  if (state.jobs !== undefined) {
    try {
      state.jobs.close()
      state.jobs = undefined
    } catch (cleanupError) {
      cleanupFailures.push(`jobs: ${describe(cleanupError)}`)
    }
  }
  if (state.monitor !== undefined) {
    try {
      state.monitor.close()
      state.monitor = undefined
    } catch (cleanupError) {
      cleanupFailures.push(`monitor: ${describe(cleanupError)}`)
    }
  }
  if (state.hub !== undefined) {
    try {
      state.hub.closeAll('startup failed')
    } catch (cleanupError) {
      cleanupFailures.push(`subscriptions: ${describe(cleanupError)}`)
    }
  }
  if (state.terminal !== undefined) {
    try {
      await withTimeout(state.terminal.closeAll(), 3000, 'terminal cleanup')
      state.terminal = undefined
    } catch (cleanupError) {
      cleanupFailures.push(`terminals: ${describe(cleanupError)}`)
    }
  }
  let scheduleCleanupFailed = false
  if (state.scheduleFiber !== undefined) {
    try {
      state.scheduleBridge?.close()
      state.scheduleBridge = undefined
      await withTimeout(state.scheduleFiber.dispose(), 3000, 'schedule dispose')
      state.scheduleFiber = undefined
    } catch (cleanupError) {
      scheduleCleanupFailed = true
      cleanupFailures.push(`schedule: ${describe(cleanupError)}`)
    }
  }
  if (state.scheduleLease !== undefined) {
    if (scheduleCleanupFailed) {
      // A scheduler that may still be live must keep its lease. The pid is
      // about to exit, so the next host sweeps it by liveness like a crash.
      cleanupFailures.push('schedule lease retained because the scheduler did not stop')
    } else {
      try {
        releaseScheduleLease(state.scheduleLease)
        state.scheduleLease = undefined
      } catch (cleanupError) {
        cleanupFailures.push(`schedule lease: ${describe(cleanupError)}`)
      }
    }
  }
  if (state.agentHandle !== undefined) {
    if (state.session !== undefined) {
      try {
        const flushed = await withTimeout(state.ctx.sessions.flush(state.session), 3000, 'flush')
        if (flushed !== true) throw new Error('session flush reported failure')
      } catch (cleanupError) {
        cleanupFailures.push(`agent flush: ${describe(cleanupError)}`)
      }
    }
    try {
      await withTimeout(state.agentHandle.dispose(), 3000, 'dispose')
    } catch (cleanupError) {
      cleanupFailures.push(`agent dispose: ${describe(cleanupError)}`)
    }
    state.agentHandle = undefined
  }

  if (cleanupFailures.length === 0) {
    if (state.claim !== undefined) {
      state.claimReleasable = true
      try {
        await withTimeout(releaseMount(state.claim), 3000, 'ledger release')
        state.claim = undefined
      } catch (cleanupError) {
        cleanupFailures.push(`ledger release: ${describe(cleanupError)}`)
        state.claimReleasable = false
      }
    }
  } else {
    // The cleanup facts are unverified. Keep the record for pid-based recovery.
    state.claimReleasable = false
    cleanupFailures.push('mount claim retained for pid-based recovery')
  }

  if (cleanupFailures.length > 0) {
    code = 'cleanup-failed'
    message = `${startupError.message}; cleanup also failed (${cleanupFailures.join('; ')})`
  }

  const params = { code, message, generation: state.config?.generation }
  if (startupError.holderPid !== undefined) params.holderPid = startupError.holderPid
  if (startupError.repairPath !== undefined) params.repairPath = startupError.repairPath
  const frame = { jsonrpc: '2.0', method: 'nano.startupError', params }
  try {
    await state.writer?.sendControl(
      encodeFrame(frame, {
        nonce: state.config?.nonce ?? state.nonce,
        requireNonce: state.transport?.nonceRequired ?? false,
      }),
    )
  } catch {
    // The transport may already be gone; diagnostics still go to stderr.
  }
  process.stderr.write(`nano-bridge: ${code}: ${message}\n`)
  await finishExit(state, 1)
}

function installSignalHandlers(state) {
  // The dsh launcher registers SIGTERM/SIGINT handlers before the profile tree
  // mounts; its handler disposes the tree and exits, which would preempt the
  // ordered bridge cleanup. The Nano host owns these signals for this process:
  // remove the earlier listeners and install the one ordered path. appExit still
  // uses the launcher's disposal controller after our cleanup.
  for (const signal of ['SIGINT', 'SIGTERM']) process.removeAllListeners(signal)
  for (const signal of ['SIGINT', 'SIGTERM', 'SIGHUP']) {
    process.on(signal, () => {
      state.signalCount += 1
      if (state.signalCount > 1) {
        process.stderr.write(`nano-bridge: second ${signal}; forcing exit without a verified cleanup\n`)
        try {
          process.kill(process.pid, 'SIGKILL')
        } catch {
          process.exit(130)
        }
        return
      }
      if (!state.ready) {
        void failStartup(state, new StartupError('startup-failed', `startup interrupted by ${signal}`))
        return
      }
      void beginShutdown(state, `signal ${signal}`, undefined, 0)
    })
  }
  process.on('disconnect', () => {
    if (!state.shuttingDown) void beginShutdown(state, 'parent process disconnected', undefined, 1)
  })
}

function onTransportWriteError(state, error) {
  if (state.shuttingDown) return
  if (!state.ready) return
  void beginShutdown(state, `transport write failed: ${describe(error)}`, undefined, 1)
}

function isRecoverable(error) {
  return error.code === -32700 || error.code === -32600 || error.code === -32602
}

function toStartupFailure(error) {
  if (error instanceof StartupError) return error
  if (error instanceof ProtocolViolation) {
    return new StartupError('startup-failed', `protocol error: ${error.message}`, { cause: error })
  }
  return new StartupError('startup-failed', describe(error), { cause: error })
}

class StepTimeout extends Error {}

function withTimeout(promise, ms, label) {
  let timer
  const timeout = new Promise((_, reject) => {
    timer = setTimeout(() => reject(new StepTimeout(`${label} exceeded ${String(ms)}ms`)), ms)
  })
  return Promise.race([promise, timeout]).finally(() => clearTimeout(timer))
}

function delay(ms) {
  return new Promise((resolve) => setTimeout(resolve, ms))
}

function describe(error) {
  if (error instanceof Error) return error.message
  return String(error)
}

function truncate(text, max) {
  return text.length <= max ? text : `${text.slice(0, max)}…`
}
