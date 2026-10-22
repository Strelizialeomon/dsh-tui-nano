// Contract conformance for the TypeScript side. The JSON contract is the
// source of truth; this test fails when the bridge's declared method set,
// limits, error codes, lifecycle edges, or fixtures drift from it.
import test from 'node:test'
import assert from 'node:assert/strict'

import {
  CONTRACT,
  ERROR_CODES,
  EVENT_KINDS,
  IMPLEMENTED_METHODS as CONTRACT_IMPLEMENTED,
  LIFECYCLE_EDGES,
  LIMITS,
  RESERVED_METHODS,
} from '../../protocol/index.mjs'
import {
  IMPLEMENTED_CLIENT_NOTIFICATION_METHODS,
  IMPLEMENTED_METHODS,
  IMPLEMENTED_NOTIFICATION_METHODS,
  IMPLEMENTED_REQUEST_METHODS,
} from '../runtime/methods.mjs'
import { buildCapabilities } from '../runtime/capabilities.mjs'
import { SHUTDOWN_STEP_NAMES } from '../runtime/host.mjs'
import {
  encodeFrame,
  parseFrameLine,
  validateEvent,
  validateHello,
  validateStartupError,
  validateStreamEnded,
} from '../runtime/frames.mjs'

test('implemented methods match the contract', () => {
  assert.deepEqual([...IMPLEMENTED_METHODS].sort(), [...CONTRACT_IMPLEMENTED.keys()].sort())
  for (const [name, method] of CONTRACT_IMPLEMENTED) {
    assert.ok(
      ['S01', 'S02', 'S03', 'S04', 'S05', 'S06', 'S07', 'S08', 'S09', 'S10', 'S11'].includes(method.owner),
      `${name} owner`,
    )
    if (method.kind === 'request') {
      assert.ok(IMPLEMENTED_REQUEST_METHODS.includes(name), `${name} must be dispatched as a request`)
    } else if (method.direction === 'host-to-client') {
      assert.ok(IMPLEMENTED_NOTIFICATION_METHODS.includes(name), `${name} must be emitted as a notification`)
    } else {
      assert.ok(
        IMPLEMENTED_CLIENT_NOTIFICATION_METHODS.includes(name),
        `${name} must be accepted as a client notification`,
      )
    }
  }
})

test('reserved methods have non-S01 owners', () => {
  for (const [name, method] of RESERVED_METHODS) {
    assert.notEqual(method.owner, 'S01', `${name} is reserved but owned by S01`)
    assert.ok(['S03', 'S06', 'S09'].includes(method.owner), `${name} owner`)
  }
})

test('limits and error codes match the contract', () => {
  assert.equal(LIMITS.maxFrameBytes, 33554432)
  assert.equal(LIMITS.maxPendingRequests, 64)
  assert.equal(LIMITS.maxAttachmentChunkBytes, 49152)
  assert.equal(LIMITS.gracefulShutdownMs, 10000)
  assert.equal(LIMITS.termGraceMs, 3000)
  assert.equal(LIMITS.mountLockStaleMs, 30000)
  assert.equal(LIMITS.maxSubmitTextBytes, 32768)
  assert.equal(LIMITS.maxSkillPreviewBytes, 65536)
  assert.equal(ERROR_CODES.get('nano/conversation/too-large'), 1018)
  assert.equal(ERROR_CODES.get('nano/terminal/unavailable'), 1049)
  assert.equal(ERROR_CODES.get('nano/terminal/input-full'), 1054)
  assert.equal(ERROR_CODES.get('nano/terminal/failed'), 1056)
  assert.equal(ERROR_CODES.get('nano/generation-mismatch'), 1005)
  assert.equal(ERROR_CODES.get('nano/unsupported'), 1008)
  assert.equal(ERROR_CODES.get('nano/closed'), 1010)
  assert.equal(ERROR_CODES.get('nano/skills/not-found'), 1019)
  assert.equal(ERROR_CODES.get('nano/skills/no-preview'), 1020)
  assert.equal(ERROR_CODES.get('nano/skills/preview-failed'), 1021)
  assert.equal(ERROR_CODES.get('nano/controls/writer-held'), 1022)
  assert.equal(ERROR_CODES.get('nano/controls/model-unavailable'), 1023)
  assert.equal(ERROR_CODES.get('nano/controls/permission-rejected'), 1024)
  assert.equal(ERROR_CODES.get('nano/controls/command-failed'), 1025)
  assert.equal(ERROR_CODES.get('nano/controls/unavailable'), 1026)
  assert.equal(ERROR_CODES.get('nano/shell/exec-failed'), 1027)
  assert.equal(ERROR_CODES.get('nano/jobs/not-found'), 1028)
  assert.equal(ERROR_CODES.get('nano/sessions/unavailable'), 1029)
  assert.equal(ERROR_CODES.get('nano/sessions/not-active'), 1030)
  assert.equal(ERROR_CODES.get('nano/sessions/title-invalid'), 1031)
  assert.equal(ERROR_CODES.get('nano/sessions/compact-unavailable'), 1032)
  assert.equal(ERROR_CODES.get('nano/sessions/rejected'), 1033)
  assert.equal(ERROR_CODES.get('nano/decisions/unknown-request'), 1045)
  assert.equal(ERROR_CODES.get('nano/decisions/answer-invalid'), 1046)
  assert.equal(ERROR_CODES.get('nano/decisions/reply-queued'), 1047)
  assert.equal(ERROR_CODES.get('nano/decisions/answer-failed'), 1048)
  assert.equal(ERROR_CODES.get('nano/monitor/unavailable'), 1057)
  assert.equal(ERROR_CODES.get('nano/monitor/not-found'), 1058)
  assert.equal(ERROR_CODES.get('nano/monitor/filter-invalid'), 1059)
  assert.equal(ERROR_CODES.get('nano/monitor/budget-invalid'), 1060)
  assert.equal(ERROR_CODES.get('nano/monitor/limit-reached'), 1061)
})

test('lifecycle edges are exactly the contract table', () => {
  for (const [from, to] of CONTRACT.lifecycle.transitions) {
    assert.ok(LIFECYCLE_EDGES.has(`${from}->${to}`))
  }
  assert.equal(LIFECYCLE_EDGES.size, CONTRACT.lifecycle.transitions.length)
})

test('event kinds match', () => {
  assert.deepEqual([...EVENT_KINDS.keys()], [
    'session.event',
    'conversation.status',
    'conversation.inbox',
    'conversation.stream',
    'schedule.changed',
    'controls.model',
    'controls.permission',
    'sessions.context',
    'skills.changed',
    'instructions.changed',
    'jobs.event',
    'jobs.output',
    'decisions.approval',
    'decisions.question',
    'decisions.projection',
    'monitor.updated',
  ])
})

test('the event shape enum and the event kind table agree', () => {
  const shapeKinds = CONTRACT.shapes.event.kind.replace(/^enum:/, '').split('|')
  assert.deepEqual([...shapeKinds].sort(), [...EVENT_KINDS.keys()].sort())
})

test('request fixture parses as a request', () => {
  const frame = parseFrameLine(JSON.stringify(CONTRACT.fixtures.request))
  assert.equal(frame.type, 'request')
  assert.equal(frame.method, 'nano.subscribe')
  assert.equal(frame.params.address.rootSessionId, 'session-fixture')
})

test('response and error fixtures parse', () => {
  const response = parseFrameLine(JSON.stringify(CONTRACT.fixtures.response))
  assert.equal(response.type, 'response')
  assert.equal(response.id, 'req-1')
  assert.ok(response.result)
  const error = parseFrameLine(JSON.stringify(CONTRACT.fixtures.errorResponse))
  assert.equal(error.type, 'response')
  assert.equal(error.error.code, 1001)
})

test('hello fixture validates and echoes contract limits', () => {
  const frame = parseFrameLine(JSON.stringify(CONTRACT.fixtures.hello))
  assert.equal(frame.type, 'notification')
  assert.equal(frame.method, 'nano.hello')
  const hello = validateHello(frame.params)
  assert.equal(hello.rootSessionId, 'session-fixture')
  assert.equal(hello.limits.maxFrameBytes, LIMITS.maxFrameBytes)
  assert.equal(hello.capabilities['tool-ask-user'].mode, 'blocking')
  assert.equal(hello.capabilities.fileReferences.status, 'supported')
  assert.equal(hello.attachmentLimits.maxImageBytes, 20 * 1024 * 1024)
  assert.equal(hello.attachmentLimits.fileAdmission, 'verbatim')
})

test('startupError, event, and streamEnded fixtures validate', () => {
  const startup = parseFrameLine(JSON.stringify(CONTRACT.fixtures.startupError))
  assert.equal(validateStartupError(startup.params).code, 'occupied')
  const event = parseFrameLine(JSON.stringify(CONTRACT.fixtures.event))
  assert.equal(validateEvent(event.params, { runtimeId: 'rt-fixture', generation: 1 }).kind, 'session.event')
  const ended = parseFrameLine(JSON.stringify(CONTRACT.fixtures.streamEnded))
  assert.equal(validateStreamEnded(ended.params, { runtimeId: 'rt-fixture', generation: 1 }).outcome, 'cancelled')
})

test('monitor fixture validates as an event', () => {
  const frame = parseFrameLine(JSON.stringify(CONTRACT.fixtures.monitorEvent))
  assert.equal(frame.type, 'notification')
  assert.equal(frame.method, 'nano.event')
  assert.equal(frame.params.kind, 'monitor.updated')
  assert.equal(frame.params.payload.monitor.role, 'user')
  assert.equal(frame.params.payload.monitor.source, 'monitor')
  assert.equal(frame.params.payload.monitor.deliveries[0].state, 'accepted')
})

test('shutdown report fixture has the four facts', () => {
  const report = CONTRACT.fixtures.shutdownResult.result
  assert.equal(report.clean, true)
  const names = report.steps.map((step) => step.name)
  for (const name of ['accepting', 'subscriptions', 'agents', 'persistence', 'dispose', 'ledger']) {
    assert.ok(names.includes(name), `missing ${name}`)
  }
  assert.ok(report.steps.every((step) => step.status === 'stopped'))
})

test('conversation fixtures parse on the bridge side', () => {
  const submit = parseFrameLine(JSON.stringify(CONTRACT.fixtures.conversationSubmit))
  assert.equal(submit.type, 'request')
  assert.equal(submit.method, 'nano.conversation.submit')
  assert.equal(submit.params.delivery, 'queue')
  assert.equal(submit.params.content[0].type, 'text')
  const withAttachments = parseFrameLine(
    JSON.stringify(CONTRACT.fixtures.conversationSubmitAttachments),
  )
  assert.equal(withAttachments.type, 'request')
  assert.equal(withAttachments.params.content[1].type, 'image')
  assert.equal(withAttachments.params.content[1].receiptId, 'receipt-image-fixture')
  assert.equal(withAttachments.params.content[2].type, 'file')
  const event = parseFrameLine(JSON.stringify(CONTRACT.fixtures.conversationEvent))
  assert.equal(event.type, 'notification')
  const validated = validateEvent(event.params, { runtimeId: 'rt-fixture', generation: 1 })
  assert.equal(validated.kind, 'conversation.status')
  assert.equal(validated.payload.status, 'running')
})

test('terminal fixtures parse on the bridge side', () => {
  const attach = parseFrameLine(JSON.stringify(CONTRACT.fixtures.terminalAttach))
  assert.equal(attach.type, 'request')
  assert.equal(attach.method, 'nano.terminal.attach')
  assert.equal(attach.params.sessionId, 'session-fixture')
  assert.equal(attach.params.terminalId, 'term-fixture')
  const result = parseFrameLine(JSON.stringify(CONTRACT.fixtures.terminalAttachResult))
  assert.equal(result.type, 'response')
  assert.equal(result.result.info.state, 'running')
  assert.equal(result.result.writable, true)
  assert.equal(typeof result.result.snapshot, 'string')
  const output = parseFrameLine(JSON.stringify(CONTRACT.fixtures.terminalOutput))
  assert.equal(output.type, 'notification')
  assert.equal(output.params.sequence, 7)
})

test('sessions fixtures parse on the bridge side', () => {
  const list = CONTRACT.fixtures.sessionsList
  assert.equal(list.result.sessions[0].sessionId, 'session-fixture')
  assert.equal(list.result.sessions[0].current, true)
  const event = parseFrameLine(JSON.stringify(CONTRACT.fixtures.sessionsContextEvent))
  const validated = validateEvent(event.params, { runtimeId: 'rt-fixture', generation: 1 })
  assert.equal(validated.kind, 'sessions.context')
  assert.equal(validated.payload.projectedTokens, 1200)
  assert.equal(validated.payload.contextWindow, 128000)
})

test('shell and jobs fixtures parse on the bridge side', () => {
  const shell = parseFrameLine(JSON.stringify(CONTRACT.fixtures.shellExec))
  assert.equal(shell.type, 'request')
  assert.equal(shell.method, 'nano.shell.exec')
  assert.equal(shell.params.command, 'printf hello')
  const jobs = parseFrameLine(JSON.stringify(CONTRACT.fixtures.jobsEvent))
  assert.equal(jobs.type, 'notification')
  const validated = validateEvent(jobs.params, { runtimeId: 'rt-fixture', generation: 1 })
  assert.equal(validated.kind, 'jobs.event')
  assert.equal(validated.payload.type, 'settled')
  assert.equal(validated.payload.job.status, 'completed')
})

test('skill and instruction fixtures parse on the bridge side', () => {
  const list = parseFrameLine(JSON.stringify(CONTRACT.fixtures.skillsList))
  assert.equal(list.type, 'request')
  assert.equal(list.method, 'nano.skills.list')
  assert.equal(list.params.address.rootSessionId, 'session-fixture')
  const listed = CONTRACT.fixtures.skillsListResult.result
  assert.equal(listed.skills[0].name, 'hello-skill')
  assert.equal(listed.skills[0].modelInvocable, false)
  assert.ok(listed.commands.includes('compact'))
  const preview = CONTRACT.fixtures.skillPreviewResult.result
  assert.equal(preview.limit, LIMITS.maxSkillPreviewBytes)
  assert.equal(preview.truncated, false)
  const event = parseFrameLine(JSON.stringify(CONTRACT.fixtures.instructionsEvent))
  const validated = validateEvent(event.params, { runtimeId: 'rt-fixture', generation: 1 })
  assert.equal(validated.kind, 'instructions.changed')
  assert.equal(validated.payload.sources[0].path, 'AGENTS.md')
  assert.match(validated.payload.budgetNotice, /^Workspace instruction budget/)
})

test('encodeFrame produces one newline-terminated frame', () => {
  const bytes = encodeFrame(CONTRACT.fixtures.request)
  assert.equal(bytes.at(-1), 0x0a)
  assert.equal(bytes.toString('utf8').trim().split('\n').length, 1)
})

test('capability names match the contract and unanswered seams are unavailable', () => {
  const capabilities = buildCapabilities({ get: () => undefined }, {})
  assert.deepEqual(Object.keys(capabilities).sort(), [...CONTRACT.capabilityNames].sort())
  assert.equal(capabilities.approval.status, 'unsupported')
  assert.equal(capabilities.userQuestions.status, 'unsupported')
  assert.equal(capabilities['tool-ask-user'].status, 'unsupported')
  assert.equal(capabilities.schedule.status, 'unsupported')
  assert.equal(capabilities.terminal.status, 'unsupported')
  const composed = buildCapabilities({ get: (key) => (key === 'terminalController' ? {} : undefined) }, {})
  assert.equal(composed.terminal.status, 'supported')
})

test('composed interaction seams report the S03 answerer as supported', () => {
  const services = new Set(['approval', 'userQuestions'])
  const capabilities = buildCapabilities(
    { get: (key) => (services.has(key) ? {} : undefined) },
    {},
  )
  assert.equal(capabilities.approval.status, 'supported')
  assert.equal(capabilities.userQuestions.status, 'supported')
  assert.equal(capabilities['tool-ask-user'].status, 'supported')
  assert.equal(capabilities['tool-ask-user'].mode, 'blocking')
})

test('decision fixtures validate as events', () => {
  for (const fixture of ['approvalEvent', 'questionEvent']) {
    const event = parseFrameLine(JSON.stringify(CONTRACT.fixtures[fixture]))
    const validated = validateEvent(event.params, { runtimeId: 'rt-fixture', generation: 1 })
    assert.match(validated.kind, /^decisions\./)
    assert.equal(validated.payload.address.rootSessionId, 'session-fixture')
  }
})

test('shutdown step names match the contract', () => {
  assert.deepEqual([...SHUTDOWN_STEP_NAMES], CONTRACT.shutdownSteps)
})
