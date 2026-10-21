// Decision host: the single answerer for the official approval and user-question
// seams, plus the timed-question claim and projection fan-out.
//
// S03 owns the card UI, but the official request objects live in this process.
// The bridge therefore publishes one pending card, waits for the Rust answer,
// and settles the waterfall with the official vocabulary. It never invents an
// allow, never answers for another agent, and never turns a silent gateway drop
// into a claimed success.
import { randomUUID } from 'node:crypto'
import { RpcError } from '../runtime/errors.mjs'

/** Named error object restored by `dsh-user-questions` as its own error type. */
function userQuestionError(code, message) {
  const error = new Error(message)
  error.name = 'UserQuestionError'
  error.code = code
  return error
}

/** Maximum tool-call arguments retained for command association. */
const MAX_INDEXED_CALLS = 2048

/**
 * One decision service per host generation.
 */
export class DecisionService {
  /**
   * @param {{ctx: object, hub: object, agent: object, session: object, rootSessionId: string}} options - host wiring.
   */
  constructor({ ctx, hub, agent, session, rootSessionId }) {
    this.ctx = ctx
    this.hub = hub
    this.agent = agent
    this.session = session
    this.rootSessionId = rootSessionId
    this.address = { rootSessionId }
    this.pendingApprovals = new Map()
    this.pendingQuestions = new Map()
    this.toolCalls = new Map()
    this.lastProjection = { active: [], settled: [] }
    this.disposers = []
    this.closed = false
    this.indexDurableCalls()
    this.listen()
  }

  /** Remember durable tool calls so a later approval can show its command. */
  indexDurableCalls() {
    for (const event of this.session.snapshotEvents()) {
      if (event.type === 'tool/call') this.indexCall(event.data)
    }
  }

  /** Subscribe to both official answerer waterfalls and the question projection. */
  listen() {
    const add = (disposer) => {
      if (typeof disposer === 'function') this.disposers.push(disposer)
    }
    const ctx = this.ctx
    if (typeof ctx.on === 'function') {
      add(ctx.on('approval/request', (request, next) => this.onApproval(request, next)))
      add(ctx.on('user-questions/request', (request, next) => this.onQuestion(request, next)))
      add(
        ctx.on('session/event', (subject, event) => {
          if (this.closed || subject?.id !== this.rootSessionId) return
          if (event?.type === 'tool/call') this.indexCall(event.data)
        }),
      )
    }
    const projections = ctx.get?.('sessionProjections')
    if (projections?.onChanged !== undefined) {
      add(
        projections.onChanged((session, key, value) => {
          if (this.closed || session?.id !== this.rootSessionId) return
          if (key !== 'userQuestions') return
          this.publishProjection(value)
        }),
      )
    }
  }

  /** Stop answering and release every claim. */
  close() {
    this.closed = true
    for (const disposer of this.disposers.splice(0)) {
      try {
        disposer()
      } catch {
        // listener disposal is best-effort during teardown
      }
    }
    for (const [requestId, pending] of [...this.pendingQuestions]) {
      this.settleQuestion(requestId, pending, { kind: 'cancelled', notify: false })
    }
    for (const [requestId, pending] of [...this.pendingApprovals]) {
      this.settleApproval(requestId, pending, 'cancelled', false)
    }
  }

  /** One approval answerer. */
  onApproval(request, next) {
    if (!this.serves(request?.agent)) return next()
    return new Promise((resolve) => {
      if (this.closed) {
        resolve('cancelled')
        return
      }
      const requestId = randomUUID()
      const pending = { request, resolve, done: false, onAbort: undefined }
      pending.onAbort = () => {
        if (pending.done) return
        this.settleApproval(requestId, pending, 'cancelled', true)
      }
      request.signal?.addEventListener('abort', pending.onAbort, { once: true })
      this.pendingApprovals.set(requestId, pending)
      this.hub.publish('decisions.approval', {
        address: this.address,
        state: 'pending',
        ...this.approvalCard(requestId, request),
      })
    })
  }

  /** One user-question answerer. */
  onQuestion(request, next) {
    if (!this.serves(request?.agent)) return next()
    return new Promise((resolve, reject) => {
      if (this.closed) {
        reject(userQuestionError('ASK_CANCELLED', 'the Nano host is shutting down'))
        return
      }
      const requestId = randomUUID()
      const callId = request.wait?.callId
      const timed = request.wait?.timed === true
      const pending = {
        request,
        resolve,
        reject,
        requestId,
        callId,
        timed,
        questions: Array.isArray(request.questions) ? request.questions : [],
        deadline: undefined,
        remainingMs: undefined,
        claimController: undefined,
        done: false,
        onAbort: undefined,
      }
      pending.onAbort = () => {
        if (pending.done) return
        const reason = request.signal?.reason
        if (reason?.code === 'ASK_TIMED_OUT') {
          this.settleQuestion(requestId, pending, { kind: 'expired' })
        } else {
          this.settleQuestion(requestId, pending, { kind: 'cancelled' })
        }
      }
      request.signal?.addEventListener('abort', pending.onAbort, { once: true })
      this.pendingQuestions.set(requestId, pending)
      this.hub.publish('decisions.question', {
        address: this.address,
        state: 'pending',
        requestId,
        ...(typeof callId === 'string' ? { callId } : {}),
        timed,
        questions: pending.questions.map(publicQuestion),
      })
      if (timed && typeof callId === 'string') {
        this.claimWait(pending)
      }
    })
  }

  /**
   * Claim the official timed wait so the Host stops counting down while this
   * UI owns the card. The claim lasts until settlement or disconnect.
   */
  claimWait(pending) {
    const userQuestions = this.ctx.get?.('userQuestions')
    if (userQuestions?.attachWait === undefined) return
    const controller = new AbortController()
    pending.claimController = controller
    void (async () => {
      try {
        for await (const frame of userQuestions.attachWait(
          this.agent,
          pending.callId,
          controller.signal,
        )) {
          if (pending.done) return
          if (typeof frame?.remainingMs !== 'number') continue
          pending.remainingMs = frame.remainingMs
          pending.deadline = Date.now() + frame.remainingMs
          this.hub.publish('decisions.question', {
            address: this.address,
            state: 'pending',
            requestId: pending.requestId,
            callId: pending.callId,
            timed: true,
            remainingMs: frame.remainingMs,
            questions: pending.questions.map(publicQuestion),
          })
        }
      } catch {
        // A claim failure leaves the Host timer in charge; the request signal
        // still settles the waterfall at its original deadline.
      }
    })()
  }

  /**
   * Answer a live approval request.
   * @param {{address?: object, requestId?: string, outcome?: string}} params - request identity and closed outcome.
   * @returns {{accepted: true}} receipt.
   */
  answerApproval(params) {
    this.assertAddress(params?.address)
    const pending = this.pendingApprovals.get(params?.requestId)
    if (pending === undefined || pending.done) {
      throw new RpcError(
        'nano/decisions/unknown-request',
        'the approval request is unknown or already settled',
      )
    }
    const outcome = params.outcome
    if (!['allowed-once', 'rejected', 'cancelled', 'unavailable'].includes(outcome)) {
      throw new RpcError(
        'nano/decisions/answer-invalid',
        'outcome must be allowed-once, rejected, cancelled, or unavailable',
      )
    }
    this.settleApproval(params.requestId, pending, outcome, true)
    return { accepted: true }
  }

  /**
   * Answer a live question, or a continued one by tool-call id.
   * @param {{address?: object, requestId?: string, callId?: string, answers?: object[], expire?: boolean, cancel?: boolean}} params - answer action.
   * @returns {{accepted: boolean, mode: string, reason?: string}} receipt.
   */
  answerQuestion(params) {
    this.assertAddress(params?.address)
    if (params.expire === true) {
      return this.expireQuestion(params)
    }
    if (params.cancel === true) {
      return this.cancelQuestion(params)
    }
    const answers = normalizeAnswers(params.answers)
    const live = params.requestId === undefined
      ? undefined
      : this.pendingQuestions.get(params.requestId)
    if (live !== undefined) {
      if (live.done) {
        throw new RpcError(
          'nano/decisions/unknown-request',
          'the question request is already settled',
        )
      }
      validateLiveBatch(live.questions, answers)
      this.settleQuestion(params.requestId, live, { kind: 'answered', answers })
      return { accepted: true, mode: 'live' }
    }
    const callId = typeof params.callId === 'string' ? params.callId : live?.callId
    if (typeof callId !== 'string' || callId === '') {
      throw new RpcError(
        'nano/decisions/unknown-request',
        'the question request is unknown; a continued answer needs callId',
      )
    }
    return this.answerContinued(callId, answers)
  }

  /** Settle a foreground timed wait as timed out. */
  expireQuestion(params) {
    const live = locateLive(this.pendingQuestions, params)
    if (live === undefined) {
      // The Host may have timed out first; the question is already continued.
      return { accepted: true, mode: 'continued', reason: 'already-continued' }
    }
    this.settleQuestion(live[0], live[1], { kind: 'expired', reject: true })
    return { accepted: true, mode: 'live' }
  }

  /** Cancel a live question wait. */
  cancelQuestion(params) {
    const live = locateLive(this.pendingQuestions, params)
    if (live === undefined) {
      return { accepted: false, mode: 'continued', reason: 'no-live-wait' }
    }
    this.settleQuestion(live[0], live[1], { kind: 'cancelled', reject: true })
    return { accepted: true, mode: 'live' }
  }

  /** Answer one continued question through the official service. */
  answerContinued(callId, answers) {
    const userQuestions = this.ctx.get?.('userQuestions')
    if (userQuestions?.answer === undefined) {
      throw new RpcError('nano/unsupported', 'the userQuestions service is not composed')
    }
    let accepted
    try {
      accepted = userQuestions.answer(this.agent, callId, { answers })
    } catch (error) {
      if (error?.code === 'REPLY_QUEUED') {
        throw new RpcError(
          'nano/decisions/reply-queued',
          'a reply is already queued for this question',
        )
      }
      if (error?.code === 'BAD_ANSWER') {
        throw new RpcError('nano/decisions/answer-invalid', error.message)
      }
      if (error?.code === 'CALLER_NOT_LIVE' || error?.code === 'DELEGATED_CALLER') {
        throw new RpcError('nano/unsupported', error.message)
      }
      throw new RpcError(
        'nano/decisions/answer-failed',
        error instanceof Error ? error.message : String(error),
      )
    }
    if (accepted !== true) {
      return { accepted: false, mode: 'continued', reason: 'not-continued' }
    }
    this.hub.publish('decisions.question', {
      address: this.address,
      state: 'submitted',
      callId,
    })
    return { accepted: true, mode: 'continued' }
  }

  /**
   * Absolute decision state after a reconnect: pending cards plus the timed
   * question projection.
   * @param {{address?: object}} params - address.
   * @returns {{address: object, approvals: object[], questions: object[], projection: object}} snapshot.
   */
  snapshot(params) {
    this.assertAddress(params?.address)
    const approvals = []
    for (const [requestId, pending] of this.pendingApprovals) {
      if (pending.done) continue
      approvals.push(this.approvalCard(requestId, pending.request))
    }
    const questions = []
    for (const pending of this.pendingQuestions.values()) {
      if (pending.done) continue
      const remainingMs = pending.deadline === undefined
        ? undefined
        : Math.max(0, pending.deadline - Date.now())
      questions.push({
        requestId: pending.requestId,
        ...(typeof pending.callId === 'string' ? { callId: pending.callId } : {}),
        timed: pending.timed,
        ...(remainingMs === undefined ? {} : { remainingMs }),
        questions: pending.questions.map(publicQuestion),
      })
    }
    return {
      address: this.address,
      approvals,
      questions,
      projection: this.projectionView(),
    }
  }

  /** The current timed-question projection. */
  projectionView() {
    const projections = this.ctx.get?.('sessionProjections')
    const state = projections?.stateOf?.(this.session, 'userQuestions')
    const questions = state?.questions
    if (questions !== undefined) return questions
    return this.lastProjection
  }

  /** Publish one projection value and remember it for reconnect snapshots. */
  publishProjection(value) {
    if (value === null || typeof value !== 'object') return
    const projection = {
      active: Array.isArray(value.active) ? value.active : [],
      settled: Array.isArray(value.settled) ? value.settled : [],
    }
    this.lastProjection = projection
    this.hub.publish('decisions.projection', {
      address: this.address,
      ...projection,
    })
  }

  settleApproval(requestId, pending, outcome, notify) {
    if (pending.done) return
    pending.done = true
    pending.request.signal?.removeEventListener('abort', pending.onAbort)
    this.pendingApprovals.delete(requestId)
    pending.resolve(outcome)
    if (notify && !this.closed) {
      this.hub.publish('decisions.approval', {
        address: this.address,
        state: 'settled',
        requestId,
        ...this.approvalCard(requestId, pending.request),
        outcome,
      })
    }
  }

  settleQuestion(requestId, pending, { kind, answers, reject }) {
    if (pending.done) return
    pending.done = true
    pending.request.signal?.removeEventListener('abort', pending.onAbort)
    this.pendingQuestions.delete(requestId)
    pending.claimController?.abort()
    if (kind === 'answered') {
      pending.resolve({ answers: answers.map(wireAnswer) })
    } else {
      pending.reject(
        userQuestionError(
          kind === 'expired' ? 'ASK_TIMED_OUT' : 'ASK_CANCELLED',
          kind === 'expired'
            ? 'ask_user_question timed out before the user answered'
            : 'the user cancelled ask_user_question',
        ),
      )
    }
    void reject
    if (kind === 'expired') {
      // The host maps ASK_TIMED_OUT to a pending result; the question becomes
      // continued in the projection and stays answerable through callId.
      if (!this.closed) {
        this.hub.publish('decisions.question', {
          address: this.address,
          state: 'expired',
          requestId,
          ...(typeof pending.callId === 'string' ? { callId: pending.callId } : {}),
        })
      }
      return
    }
    if (!this.closed) {
      this.hub.publish('decisions.question', {
        address: this.address,
        state: kind === 'answered' ? 'settled' : 'cancelled',
        requestId,
        ...(typeof pending.callId === 'string' ? { callId: pending.callId } : {}),
        ...(kind === 'answered' ? { answers: answers.map(wireAnswer) } : {}),
      })
    }
  }

  approvalCard(requestId, request) {
    return {
      requestId,
      toolName: typeof request.toolName === 'string' ? request.toolName : 'tool',
      ...(typeof request.reason === 'string' ? { reason: request.reason } : {}),
      ...(request.displayReason === undefined ? {} : { displayReason: request.displayReason }),
      ...(typeof request.callId === 'string' ? { callId: request.callId } : {}),
      ...(typeof request.callId === 'string'
        ? commandField(this.commandFor(request.callId))
        : {}),
    }
  }

  indexCall(data) {
    const callId = data?.callId
    const name = data?.name
    const argumentsText = data?.arguments
    if (typeof callId !== 'string' || callId === '' || typeof name !== 'string') return
    this.toolCalls.set(callId, {
      name,
      arguments: typeof argumentsText === 'string' ? argumentsText : '',
    })
    while (this.toolCalls.size > MAX_INDEXED_CALLS) {
      const oldest = this.toolCalls.keys().next().value
      this.toolCalls.delete(oldest)
    }
  }

  commandFor(callId) {
    if (typeof callId !== 'string') return undefined
    const call = this.toolCalls.get(callId)
    if (call === undefined) return undefined
    let value
    try {
      value = JSON.parse(call.arguments)
    } catch {
      return undefined
    }
    return typeof value?.command === 'string' ? value.command : undefined
  }

  serves(agent) {
    return agent !== undefined && agent !== null && agent.id === this.rootSessionId
  }

  assertAddress(address) {
    if (address === null || typeof address !== 'object' || Array.isArray(address)) {
      throw new RpcError('jsonrpc/invalid-params', 'address must be an object')
    }
    if (typeof address.rootSessionId !== 'string' || address.rootSessionId === '') {
      throw new RpcError('jsonrpc/invalid-params', 'address.rootSessionId must be a non-empty string')
    }
    if (address.rootSessionId !== this.rootSessionId) {
      throw new RpcError('nano/unsupported', 'address names a root session this host does not serve')
    }
    if (address.childSessionId !== undefined) {
      throw new RpcError('nano/unsupported', 'child-session addresses belong to S12')
    }
  }
}

function commandField(command) {
  return typeof command === 'string' ? { command } : {}
}

function locateLive(pending, params) {
  if (typeof params.requestId === 'string') {
    const value = pending.get(params.requestId)
    return value === undefined ? undefined : [params.requestId, value]
  }
  return findPendingByCall(pending, params.callId)
}

function findPendingByCall(pending, callId) {
  if (typeof callId !== 'string') return undefined
  for (const [requestId, value] of pending) {
    if (value.callId === callId && !value.done) return [requestId, value]
  }
  return undefined
}

/** Keep the public question shape; unknown members pass through untouched. */
function publicQuestion(question) {
  return question
}

function normalizeAnswers(answers) {
  if (!Array.isArray(answers) || answers.length === 0) {
    throw new RpcError('nano/decisions/answer-invalid', 'answers must be a non-empty array')
  }
  return answers.map((item) => {
    if (item === null || typeof item !== 'object' || Array.isArray(item)) {
      throw new RpcError('nano/decisions/answer-invalid', 'each answer must be an object')
    }
    if (typeof item.id !== 'string' || item.id === '') {
      throw new RpcError('nano/decisions/answer-invalid', 'answer.id must be a non-empty string')
    }
    if (!Array.isArray(item.selected) || item.selected.some((label) => typeof label !== 'string')) {
      throw new RpcError('nano/decisions/answer-invalid', 'answer.selected must be a string array')
    }
    if (item.custom !== undefined && typeof item.custom !== 'string') {
      throw new RpcError('nano/decisions/answer-invalid', 'answer.custom must be a string')
    }
    return {
      id: item.id,
      selected: [...item.selected],
      ...(item.custom === undefined || item.custom === '' ? {} : { custom: item.custom }),
    }
  })
}

function wireAnswer(answer) {
  return {
    id: answer.id,
    selected: [...answer.selected],
    ...(answer.custom === undefined || answer.custom === '' ? {} : { custom: answer.custom }),
  }
}

/** Live answers must name each question exactly once. */
function validateLiveBatch(questions, answers) {
  if (answers.length !== questions.length) {
    throw new RpcError(
      'nano/decisions/answer-invalid',
      `the batch has ${String(answers.length)} answers for ${String(questions.length)} questions`,
    )
  }
  const ids = new Set()
  for (const answer of answers) {
    if (ids.has(answer.id)) {
      throw new RpcError('nano/decisions/answer-invalid', `duplicate answer id ${answer.id}`)
    }
    ids.add(answer.id)
    if (!questions.some((question) => question.id === answer.id)) {
      throw new RpcError('nano/decisions/answer-invalid', `unknown question id ${answer.id}`)
    }
  }
}
