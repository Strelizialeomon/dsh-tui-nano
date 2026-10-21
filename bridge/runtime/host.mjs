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
import { SkillsService } from '../skills/service.mjs'
import { buildCapabilities } from './capabilities.mjs'
import { IMPLEMENTED_REQUEST_METHODS } from './methods.mjs'

const BRIDGE_PACKAGE = JSON.parse(fs.readFileSync(new URL('../package.json', import.meta.url), 'utf8'))
export const BRIDGE_VERSION = BRIDGE_PACKAGE.version
const SUPPORTED_OFFICIAL_VERSION = '0.2.1-rc.2'
export const SHUTDOWN_STEP_NAMES = CONTRACT.shutdownSteps

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
    skills: undefined,
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
    throw new StartupError('startup-failed', `could not ${state.config.sessionMode} root session: ${describe(error)}`, {
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
  })
  state.conversation = new ConversationService({
    ctx,
    hub: state.hub,
    agent: state.agentHandle.agent,
    session,
    rootSessionId: state.config.rootSessionId,
  })
  state.skills = new SkillsService({
    ctx,
    hub: state.hub,
    agent: state.agentHandle.agent,
    session,
    rootSessionId: state.config.rootSessionId,
    cwd,
  })

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
      capabilities: buildCapabilities(ctx, state.config),
      limits: {
        maxFrameBytes: LIMITS.maxFrameBytes,
        maxPendingRequests: LIMITS.maxPendingRequests,
        maxAttachmentChunkBytes: LIMITS.maxAttachmentChunkBytes,
      },
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

/** Build the optional initial Agent route from the runtime patch. */
function agentRoute(config) {
  if (config.provider === undefined) return undefined
  const route = { provider: config.provider, model: config.model }
  if (config.reasoningEffort !== undefined) route.reasoningEffort = config.reasoningEffort
  return route
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
    // S01 has no client-to-host notifications. Reserved notification methods are
    // rejected silently (notifications carry no response by definition).
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
    cancellable: frame.method === 'nano.subscribe',
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
    case 'nano.conversation.submit': {
      await sendResult(state, id, state.conversation.submit(params))
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
    state.conversation?.close()
    state.conversation = undefined
    state.skills?.close()
    state.skills = undefined
    state.hub?.closeAll(reason)
  }, faults))

  steps.push(await runStep('agents', deadline, async () => {
    if (state.agentHandle === undefined) return
    // Cancel the main Agent subtree without unregistering the Session yet; the
    // persistence flush below still needs a live Session in the store.
    state.agentHandle.agent.cancel({ kind: 'user' })
    await state.agentHandle.agent.whenIdle()
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
  if (state.conversation !== undefined) {
    try {
      state.conversation.close()
      state.conversation = undefined
    } catch (cleanupError) {
      cleanupFailures.push(`conversation: ${describe(cleanupError)}`)
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
  if (state.hub !== undefined) {
    try {
      state.hub.closeAll('startup failed')
    } catch (cleanupError) {
      cleanupFailures.push(`subscriptions: ${describe(cleanupError)}`)
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
