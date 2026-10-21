// Official job observation for the human UI.
//
// S08 never owns a second task table: every projection, output byte, and stop
// result comes from `ctx.jobs`. Observation is non-consuming (`readAt`), so
// opening or scrolling a task never moves the model's `job_output` cursor and
// never suppresses its completion notice. The registry owns stop requests; the
// bridge only forwards the human reason and reports the request, not an exit.
import { RpcError } from '../runtime/errors.mjs'

const HUMAN_STOP_REASON = 'cancelled by the user'

/**
 * One jobs observation service per host generation.
 */
export class JobsService {
  /**
   * @param {{
   *   ctx: object,
   *   hub: import('../runtime/events.mjs').SubscriptionHub,
   *   session: object,
   *   rootSessionId: string,
   * }} options - host wiring.
   */
  constructor({ ctx, hub, session, rootSessionId }) {
    this.ctx = ctx
    this.hub = hub
    this.session = session
    this.rootSessionId = rootSessionId
    this.address = { rootSessionId }
    this.registry = ctx.get('jobs')
    this.closed = false
    this.toolCalls = new Map()
    this.disposers = []
    this.indexRegisteredJobs()
    this.listen()
  }

  /** Remember `job/registered` tool-call correlation; JobView does not carry it. */
  indexRegisteredJobs() {
    for (const event of collectSessionEvents(this.session)) {
      if (event.type !== 'job/registered') continue
      this.rememberJob(event.data)
    }
  }

  /** Subscribe to the official registry and the durable registration log. */
  listen() {
    const on = (event, handler) => {
      const disposer = this.ctx.on(event, handler)
      if (typeof disposer === 'function') this.disposers.push(disposer)
    }
    on('session/event', (subject, event) => {
      if (this.closed || subject.id !== this.rootSessionId || event.type !== 'job/registered') return
      this.rememberJob(event.data)
    })
    if (this.registry !== undefined) {
      try {
        this.disposers.push(this.registry.events.subscribe({ owner: this.rootSessionId }, (event) => {
          this.publish(event)
        }))
      } catch {
        // An implementation without an event stream still supports snapshots.
      }
      try {
        this.disposers.push(this.registry.attachController('nano-jobs'))
      } catch {
        // Another controller already serves this owner; observation still works.
      }
    }
  }

  /** Stop publishing and detach the controller; live work belongs to the owner. */
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

  /** Publish one official job event without inventing state. */
  publish(event) {
    if (this.closed) return
    if (event.type === 'output') {
      const payload = { address: this.address, jobId: String(event.id), total: event.total }
      if (event.owner !== undefined) payload.owner = event.owner
      this.hub.publish('jobs.output', payload)
      return
    }
    const payload = {
      address: this.address,
      type: event.type,
      job: this.enrich(event.job),
    }
    if (event.cause !== undefined) payload.cause = event.cause
    if (typeof event.awaited === 'boolean') payload.awaited = event.awaited
    this.hub.publish('jobs.event', payload)
  }

  /**
   * List every job this root session may see (its own plus unowned).
   * @param {object} params - `address`.
   * @returns {{jobs: object[]}} enriched official projections.
   */
  snapshot(params) {
    this.assertAvailable()
    this.assertAddress(params?.address)
    return { jobs: this.registry.list(this.rootSessionId).map((job) => this.enrich(job)) }
  }

  /**
   * Read retained output at an absolute byte offset without consuming it.
   * @param {object} params - `address`, `jobId`, `from`.
   * @returns {{chunks: object[], next: number, lossy: boolean}} one non-consuming read.
   */
  follow(params) {
    this.assertAvailable()
    this.assertAddress(params?.address)
    const jobId = validateJobId(params.jobId)
    if (!Number.isInteger(params.from) || params.from < 0) {
      throw new RpcError('jsonrpc/invalid-params', 'from must be a non-negative integer')
    }
    try {
      const read = this.registry.readAt(jobId, params.from, this.rootSessionId)
      return { chunks: read.chunks, next: read.next, lossy: read.lossy === true }
    } catch (error) {
      throw notFound(error)
    }
  }

  /**
   * Request cancellation with the human reason. The result is a request fact,
   * never proof that the process exited; the terminal state arrives as an
   * official `jobs.event` projection.
   * @param {object} params - `address`, `jobId`.
   * @returns {{outcome: 'requested'|'already-finished'}} registry answer.
   */
  kill(params) {
    this.assertAvailable()
    this.assertAddress(params?.address)
    const jobId = validateJobId(params.jobId)
    try {
      const outcome = this.registry.kill(jobId, this.rootSessionId, HUMAN_STOP_REASON)
      return { outcome: outcome === 'already-finished' ? 'already-finished' : 'requested' }
    } catch (error) {
      throw notFound(error)
    }
  }

  /** Attach the durable tool-call correlation the JobView shape omits. */
  enrich(job) {
    if (job === null || typeof job !== 'object') return job
    const toolCall = this.toolCalls.get(String(job.id))
    if (toolCall === undefined) return job
    return { ...job, toolCall }
  }

  /** Record one `job/registered` payload. */
  rememberJob(data) {
    if (data === null || typeof data !== 'object') return
    const jobId = typeof data.jobId === 'string' ? data.jobId : undefined
    if (jobId === undefined) return
    const toolCall = data.toolCall
    if (
      toolCall !== undefined &&
      typeof toolCall.callId === 'string' &&
      typeof toolCall.rootCallId === 'string'
    ) {
      this.toolCalls.set(jobId, { callId: toolCall.callId, rootCallId: toolCall.rootCallId })
    }
    // Bound the correlation cache; settled registrations are the only growth.
    while (this.toolCalls.size > 4096) {
      const oldest = this.toolCalls.keys().next().value
      this.toolCalls.delete(oldest)
    }
  }

  assertAvailable() {
    if (this.registry === undefined) {
      throw new RpcError('nano/unsupported', 'the official job registry is not composed')
    }
  }

  /** Only the exact root address is served. */
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

/** Read durable events from a session implementation when it exposes a snapshot. */
function collectSessionEvents(session) {
  if (typeof session?.snapshotEvents !== 'function') return []
  try {
    return session.snapshotEvents()
  } catch {
    return []
  }
}

function validateJobId(jobId) {
  if (typeof jobId !== 'string' || jobId === '' || Buffer.byteLength(jobId, 'utf8') > 256) {
    throw new RpcError('jsonrpc/invalid-params', 'jobId must be a non-empty string')
  }
  return jobId
}

/** A missing, foreign, or already-removed job is indistinguishable to the caller. */
function notFound(error) {
  return new RpcError('nano/jobs/not-found', `job not visible to this session: ${describe(error)}`)
}

function describe(error) {
  return error instanceof Error ? error.message : String(error)
}
