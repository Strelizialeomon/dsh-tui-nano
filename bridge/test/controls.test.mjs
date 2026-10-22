// Controls host unit tests. The fake services mirror only the public official
// surface S04 consumes: the LLM registry, the default-model service, the
// session's durable events, the permission preset catalog, and the command
// registry. In particular the tests pin that a preset switch goes through the
// official `/permission` command and never `permissionPresets.set()`.
import test from 'node:test'
import assert from 'node:assert/strict'

import { ControlsService } from '../controls/service.mjs'

// The profile installs these official packages; a bare checkout does not.
// Tests that assert official helper behavior skip when the packages are
// absent, but an installed package with a missing/renamed export is a real
// contract drift and fails loudly. The real-host suite starts a profile where
// the helpers are installed, but it does not assert their behavior.
async function optionalOfficial(specifier) {
  try {
    return await import(specifier)
  } catch (error) {
    if (error?.code === 'ERR_MODULE_NOT_FOUND') return undefined
    throw error
  }
}

const agentModule = await optionalOfficial('@deepseek-ai/dsh-agent')
const controllerModule = await optionalOfficial('@deepseek-ai/dsh-api-session-controller')
if (agentModule !== undefined && typeof agentModule.installModelSelection !== 'function') {
  throw new Error('@deepseek-ai/dsh-agent is installed but does not export installModelSelection')
}
if (controllerModule !== undefined && typeof controllerModule.buildModelCatalog !== 'function') {
  throw new Error(
    '@deepseek-ai/dsh-api-session-controller is installed but does not export buildModelCatalog',
  )
}
if ((agentModule === undefined) !== (controllerModule === undefined)) {
  throw new Error('official bridge dependencies are only partially installed')
}
const hasOfficialHooks = agentModule !== undefined
const hasOfficialCatalog = controllerModule !== undefined
const official =
  hasOfficialHooks && hasOfficialCatalog
    ? {
        installModelSelection: agentModule.installModelSelection,
        buildModelCatalog: controllerModule.buildModelCatalog,
      }
    : {}
const OFFICIAL_SKIP = 'official bridge dependencies are not installed'

const ROOT = 'session-root'
const ADDRESS = { rootSessionId: ROOT }

function fakeContext(services = {}) {
  const handlers = new Map()
  return {
    ...services,
    handlers,
    get(key) {
      return services[key]
    },
    on(event, handler) {
      const list = handlers.get(event) ?? []
      list.push(handler)
      handlers.set(event, list)
      return () => {
        const current = handlers.get(event) ?? []
        handlers.set(event, current.filter((item) => item !== handler))
      }
    },
    emit(event, ...args) {
      for (const handler of [...(handlers.get(event) ?? [])]) handler(...args)
    },
    logger: { warn() {} },
  }
}

function fakeAgentContext() {
  const handlers = new Map()
  return {
    handlers,
    on(event, handler) {
      const list = handlers.get(event) ?? []
      list.push(handler)
      handlers.set(event, list)
      return () => {
        const current = handlers.get(event) ?? []
        handlers.set(event, current.filter((item) => item !== handler))
      }
    },
  }
}

function fakeSession({ header, events = [] } = {}) {
  return {
    id: ROOT,
    appended: [],
    snapshotEvents() {
      return events
    },
    requestHeader() {
      return header
    },
    append(type, data) {
      this.appended.push({ type, data })
    },
  }
}

function serviceFor(services = {}, { header, events, official: override } = {}) {
  const ctx = fakeContext(services)
  const session = fakeSession({ header, events })
  const agentContext = fakeAgentContext()
  const agent = { id: ROOT, ctx: agentContext }
  const published = []
  const hub = { publish: (kind, payload) => published.push({ kind, payload }) }
  const service = new ControlsService({
    ctx,
    hub,
    agent,
    session,
    rootSessionId: ROOT,
    official: override ?? official,
  })
  return { ctx, session, agent, agentContext, published, service }
}

test('state folds the durable model header and pending selection', () => {
  const services = {
    permissionPresets: {
      current: () => 'workspace-write',
      catalog: () => ({
        options: [{ value: 'workspace-write', name: 'Workspace write', sandbox: 'workspace-write', approval: 'ask' }],
        defaultOptions: [{ value: 'workspace-write', name: 'Workspace write' }],
        defaultPreset: 'workspace-write',
      }),
      resolve: () => ({ sandbox: 'workspace-write', approval: 'ask' }),
      set: () => assert.fail('permissionPresets.set must not be used'),
    },
    commands: {
      list: () => [
        { name: 'permission', description: 'Switch the permission preset', input: { hint: '<preset>' } },
        { name: 'compact', description: 'Compact the conversation' },
      ],
    },
  }
  const { service } = serviceFor(services, {
    header: { config: { provider: 'deepseek-official', model: 'deepseek-chat' } },
    events: [
      {
        type: 'model/selection',
        data: { provider: 'deepseek-official', model: 'deepseek-reasoner', reasoningEffort: 'high' },
      },
    ],
  })
  const state = service.state()
  assert.deepEqual(state.address, ADDRESS)
  assert.deepEqual(state.model, {
    lastUsed: { provider: 'deepseek-official', model: 'deepseek-chat' },
    next: { provider: 'deepseek-official', model: 'deepseek-reasoner', reasoningEffort: 'high' },
  })
  assert.equal(state.permission.current, 'workspace-write')
  assert.equal(state.permission.catalog.defaultPreset, 'workspace-write')
  assert.equal(state.permission.catalog.options[0].approval, 'ask')
  assert.equal(state.commands.length, 2)
  assert.equal(state.commands[0].inputHint, '<preset>')
})

test('a missing permission service is reported as unavailable, not invented', () => {
  const { service } = serviceFor()
  const state = service.state()
  assert.deepEqual(state.model, { lastUsed: null, next: null })
  assert.equal(state.permission.available, false)
  assert.equal(state.permission.current, null)
  assert.deepEqual(state.permission.catalog.options, [])
  assert.deepEqual(state.commands, [])
})

test('an unreadable permission projection is an error, not an empty success', () => {
  const { service } = serviceFor({
    permissionPresets: {
      current() {
        throw new Error('projection is unreadable')
      },
      catalog: () => ({ options: [], defaultOptions: [], defaultPreset: '' }),
      resolve: () => ({}),
    },
  })
  assert.throws(
    () => service.state(),
    (error) => error.rpcName === 'nano/controls/unavailable' && /unreadable/.test(error.message),
  )
})

test('the initial selection falls back to the recorded header', () => {
  const { service } = serviceFor(
    {},
    { header: { config: { provider: 'p', model: 'm', reasoningEffort: 'low' } } },
  )
  assert.deepEqual(service.selection.current, {
    provider: 'p',
    model: 'm',
    reasoningEffort: 'low',
  })
})

test('the initial selection falls back to the deployment default', () => {
  const { service } = serviceFor({
    agentDefaultModel: {
      currentSelection: () => ({ provider: 'p', model: 'm', reasoningEffort: 'high' }),
    },
  })
  assert.deepEqual(service.selection.current, {
    provider: 'p',
    model: 'm',
    reasoningEffort: 'high',
  })
})

test('a pending durable selection wins over the header and default', () => {
  const { service } = serviceFor(
    { agentDefaultModel: { currentSelection: () => ({ provider: 'default', model: 'd' }) } },
    {
      header: { config: { provider: 'p', model: 'old' } },
      events: [{ type: 'model/selection', data: { provider: 'p', model: 'new' } }],
    },
  )
  assert.deepEqual(service.selection.current, { provider: 'p', model: 'new' })
})

test('a composed session controller installs only its own selection hooks', () => {
  const { agentContext, service } = serviceFor({
    sessionController: {
      async selectModel(request) {
        return { selected: request }
      },
      async modelCatalog() {
        return { default: { provider: 'p', model: 'm' }, routableProviders: [], groups: [], failures: [] }
      },
    },
  })
  assert.equal(agentContext.handlers.size, 0)
  assert.deepEqual(service.selection.current, undefined)
})

test('command failures keep their own contract code', async () => {
  const { service } = serviceFor({
    commands: {
      list: () => [],
      async execute() {
        const error = new Error('command handler exploded')
        error.code = 'command/internal'
        throw error
      },
    },
  })
  await assert.rejects(
    service.executeCommand({ line: '/compact' }),
    (error) => error.rpcName === 'nano/controls/command-failed',
  )
  await assert.rejects(
    service.executeCommand({ line: '/permission danger-full-access' }),
    (error) => error.rpcName === 'nano/controls/permission-rejected',
  )
})

test('selectModel uses the official session controller when it is composed', async () => {
  const requests = []
  const { service } = serviceFor({
    sessionController: {
      async selectModel(request) {
        requests.push(request)
        return { selected: { ...request, reasoningEffort: request.reasoningEffort ?? 'default' } }
      },
    },
  })
  const result = await service.selectModel({
    provider: 'deepseek-official',
    model: 'deepseek-chat',
    reasoningEffort: 'max',
  })
  assert.deepEqual(requests, [
    {
      sessionId: ROOT,
      provider: 'deepseek-official',
      model: 'deepseek-chat',
      reasoningEffort: 'max',
    },
  ])
  assert.equal(result.selected.reasoningEffort, 'max')
})

test('selectModel omits an absent effort and maps writer-held rejection', async () => {
  const requests = []
  const { service } = serviceFor({
    sessionController: {
      async selectModel(request) {
        requests.push(request)
        const error = new Error('session is owned by another writer')
        error.code = 'session/writer-held'
        throw error
      },
    },
  })
  await assert.rejects(
    service.selectModel({ provider: 'p', model: 'm' }),
    (error) => error.rpcName === 'nano/controls/writer-held',
  )
  assert.deepEqual(requests, [{ sessionId: ROOT, provider: 'p', model: 'm' }])
})

test('unavailable models keep the official model-unavailable code', async () => {
  const { service } = serviceFor({
    sessionController: {
      async selectModel() {
        const error = new Error('provider is not configured')
        error.code = 'session/model-unavailable'
        throw error
      },
    },
  })
  await assert.rejects(
    service.selectModel({ provider: 'p', model: 'm' }),
    (error) => error.rpcName === 'nano/controls/model-unavailable',
  )
})

test('the CLI fallback validates and appends model/selection', async () => {
  const saved = []
  const { session, agentContext, service } = serviceFor({
    llm: {
      async listModels(provider) {
        assert.equal(provider, 'p')
        return [{ id: 'chat' }, { id: 'reasoner' }]
      },
      async resolveCallConfig(config) {
        return { ...config, reasoningEffort: config.reasoningEffort ?? 'default' }
      },
    },
    agentDefaultModel: {
      currentSelection: () => ({ provider: 'p', model: 'chat' }),
      async saveSelection(selection) {
        saved.push(selection)
      },
    },
  })
  const result = await service.selectModel({
    provider: 'p',
    model: 'reasoner',
    reasoningEffort: 'high',
  })
  assert.deepEqual(result.selected, {
    provider: 'p',
    model: 'reasoner',
    reasoningEffort: 'high',
  })
  assert.deepEqual(session.appended, [
    { type: 'model/selection', data: { provider: 'p', model: 'reasoner', reasoningEffort: 'high' } },
  ])
  await Promise.resolve()
  assert.deepEqual(saved, [
    { provider: 'p', model: 'reasoner', reasoningEffort: 'high' },
  ])
  assert.deepEqual(service.modelState().next, {
    provider: 'p',
    model: 'reasoner',
    reasoningEffort: 'high',
  })
})

test(
  'the CLI fallback installs the official selection hooks',
  { skip: hasOfficialHooks ? false : OFFICIAL_SKIP },
  () => {
    const { agentContext } = serviceFor()
    assert.deepEqual([...agentContext.handlers.keys()].sort(), [
      'agent/pre-step',
      'agent/request',
      'system-prompt/assemble',
    ])
  },
)

test('the CLI fallback refuses a model outside the official registry', async () => {
  const { session, service } = serviceFor({
    llm: {
      async listModels() {
        return [{ id: 'chat' }]
      },
      async resolveCallConfig(config) {
        return config
      },
    },
    agentDefaultModel: {
      currentSelection: () => ({ provider: 'p', model: 'chat' }),
      saveSelection: async () => {},
    },
  })
  await assert.rejects(
    service.selectModel({ provider: 'p', model: 'removed' }),
    (error) => error.rpcName === 'nano/controls/model-unavailable',
  )
  assert.deepEqual(session.appended, [])
})

test(
  'modelCatalog falls back to the official LLM registry with isolated failures',
  { skip: hasOfficialCatalog ? false : OFFICIAL_SKIP },
  async () => {
  const { service } = serviceFor({
    llm: {
      listProviders: () => [
        { id: 'p', name: 'Provider' },
        { id: 'broken', name: 'Broken' },
      ],
      async listModels(provider) {
        if (provider === 'broken') throw new Error('no credentials')
        return [{ id: 'chat', name: 'Chat' }]
      },
      async resolveModelInfo() {
        return {
          reasoning: {
            efforts: [{ id: 'low', name: 'Low' }],
            defaultEffort: 'low',
          },
        }
      },
    },
    agentDefaultModel: {
      currentSelection: () => ({ provider: 'p', model: 'chat' }),
    },
  })
  const catalog = await service.modelCatalog()
  assert.deepEqual(catalog.default, { provider: 'p', model: 'chat' })
  assert.deepEqual(catalog.routableProviders, ['p'])
  assert.equal(catalog.groups[0].models[0].reasoning.defaultEffort, 'low')
  assert.equal(catalog.failures[0].id, 'broken')
  assert.match(catalog.failures[0].message, /no credentials/)
})

test('setPermission executes the official /permission command and rereads the preset', async () => {
  const executions = []
  let current = 'workspace-write'
  const { service } = serviceFor({
    commands: {
      list: () => [],
      async execute(_agent, line, attachments) {
        executions.push({ line, attachments })
        current = 'danger-full-access'
        return {
          commandId: 'cmd-1',
          result: { kind: 'success', text: 'preset danger-full-access' },
        }
      },
    },
    permissionPresets: {
      current: () => current,
      catalog: () => ({ options: [], defaultOptions: [], defaultPreset: 'workspace-write' }),
      resolve: () => ({}),
      set: () => assert.fail('permissionPresets.set must not be used'),
    },
  })
  const result = await service.setPermission({ preset: 'danger-full-access' })
  assert.deepEqual(executions, [{ line: '/permission danger-full-access', attachments: [] }])
  assert.equal(result.executed, true)
  assert.equal(result.kind, 'success')
  assert.equal(result.text, 'preset danger-full-access')
  assert.equal(result.permission.current, 'danger-full-access')
})

test('a rejected preset keeps the official error result and the actual current value', async () => {
  let current = 'workspace-write'
  const { service } = serviceFor({
    commands: {
      list: () => [],
      async execute(_agent, line) {
        return { commandId: 'cmd-2', result: { kind: 'error', text: `unknown preset "${line}"` } }
      },
    },
    permissionPresets: {
      current: () => current,
      catalog: () => ({ options: [], defaultOptions: [], defaultPreset: 'workspace-write' }),
      resolve: () => ({}),
      set: () => assert.fail('permissionPresets.set must not be used'),
    },
  })
  const result = await service.setPermission({ preset: 'nope' })
  assert.equal(result.executed, true)
  assert.equal(result.kind, 'error')
  assert.match(result.text, /unknown preset/)
  assert.equal(result.permission.current, 'workspace-write')
})

test('executeCommand reports an unknown official name as not executed', async () => {
  const { service } = serviceFor({
    commands: {
      list: () => [],
      async execute() {
        return undefined
      },
    },
  })
  assert.deepEqual(await service.executeCommand({ line: '/nope' }), { executed: false })
})

test('executeCommand rejects a line that is not a slash command', async () => {
  const { service } = serviceFor()
  await assert.rejects(
    service.executeCommand({ line: 'hello' }),
    (error) => error.rpcName === 'jsonrpc/invalid-params',
  )
})

test('model and permission session events publish absolute projections', () => {
  let current = 'workspace-write'
  const services = {
    permissionPresets: {
      current: () => current,
      catalog: () => ({ options: [], defaultOptions: [], defaultPreset: 'workspace-write' }),
      resolve: () => ({}),
      set: () => assert.fail('permissionPresets.set must not be used'),
    },
  }
  const { ctx, session, published, service } = serviceFor(services)

  ctx.emit('session/event', session, {
    type: 'model/selection',
    data: { provider: 'p', model: 'm', reasoningEffort: 'high' },
  })
  assert.equal(published.at(-1).kind, 'controls.model')
  assert.deepEqual(published.at(-1).payload.model.next, {
    provider: 'p',
    model: 'm',
    reasoningEffort: 'high',
  })

  ctx.emit('session/event', session, {
    type: 'request/header',
    data: { header: { config: { provider: 'p', model: 'm', reasoningEffort: 'high' } } },
  })
  assert.equal(published.at(-1).kind, 'controls.model')
  assert.deepEqual(published.at(-1).payload.model.lastUsed, {
    provider: 'p',
    model: 'm',
    reasoningEffort: 'high',
  })
  assert.equal(published.at(-1).payload.model.next.reasoningEffort, 'high')

  current = 'custom'
  ctx.emit('session/event', session, { type: 'sandbox/mode', data: { mode: 'read-only' } })
  assert.equal(published.at(-1).kind, 'controls.permission')
  assert.equal(published.at(-1).payload.permission.current, 'custom')

  ctx.emit('permission-presets/catalog-changed')
  assert.equal(published.at(-1).kind, 'controls.permission')

  service.close()
  const count = published.length
  ctx.emit('session/event', session, { type: 'model/selection', data: {} })
  assert.equal(published.length, count)
})

test('adapter-default efforts are not reported as an explicit selection', () => {
  const { service } = serviceFor(
    {},
    {
      header: {
        config: { provider: 'p', model: 'm', reasoningEffort: 'high' },
        adapterDefaults: { reasoningEffort: true },
      },
    },
  )
  assert.deepEqual(service.modelState().lastUsed, { provider: 'p', model: 'm' })
})

test(
  'a provider/model change inserts the official model-change notice',
  { skip: hasOfficialHooks ? false : OFFICIAL_SKIP },
  async () => {
  const { agentContext, service } = serviceFor()
  service.selection.assembled = { provider: 'p', model: 'new' }
  const handler = agentContext.handlers.get('agent/pre-step')[0]
  const decision = await handler(
    {
      agent: { session: { requestHeader: () => ({ config: { provider: 'p', model: 'old' } }) } },
      messages: [],
      signal: { aborted: false },
      step: 2,
    },
    async () => ({ kind: 'continue', messages: [] }),
  )
  assert.equal(decision.messages.length, 1)
  assert.match(decision.messages[0].content[0].text, /model changed/)
  assert.match(decision.messages[0].content[0].text, /generated by old/)
  assert.match(decision.messages[0].content[0].text, /continues with new/)
  assert.equal(decision.messages[0].source.kind, 'model-selection')
})

test(
  'a pure effort change does not add a model-change notice',
  { skip: hasOfficialHooks ? false : OFFICIAL_SKIP },
  async () => {
  const { agentContext, service } = serviceFor()
  service.selection.assembled = { provider: 'p', model: 'm', reasoningEffort: 'high' }
  const handler = agentContext.handlers.get('agent/pre-step')[0]
  const decision = await handler(
    {
      agent: { session: { requestHeader: () => ({ config: { provider: 'p', model: 'm' } }) } },
      messages: [],
      signal: { aborted: false },
      step: 2,
    },
    async () => ({ kind: 'continue', messages: [] }),
  )
  assert.equal(decision.messages.length, 0)
})

test(
  'selection hooks are disposed when the service closes',
  { skip: hasOfficialHooks ? false : OFFICIAL_SKIP },
  () => {
  const { agentContext, service } = serviceFor()
  assert.equal(agentContext.handlers.size, 3)
  service.close()
  for (const list of agentContext.handlers.values()) assert.equal(list.length, 0)
})

test('session events for another session are ignored', () => {
  const { ctx, published } = serviceFor()
  ctx.emit('session/event', { id: 'other' }, { type: 'model/selection', data: {} })
  assert.equal(published.length, 0)
})
