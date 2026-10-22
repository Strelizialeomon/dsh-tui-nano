// Capability snapshot sent in nano.hello.
//
// The three states are deliberate: `supported` means the official service is
// composed and usable now, `unavailable` means it is composed but a dependency
// is missing at this moment, and `unsupported` means this profile does not
// compose it at all. A UI must never present an unsupported or unavailable
// capability as working.
const SERVICE_CAPABILITIES = Object.freeze({
  agent: 'agents',
  sessionPersistence: 'sessionPersistence',
  sessionQuery: 'sessionQuery',
  attachments: 'attachments',
  fileReferences: 'fileReferences',
  userQuestions: 'userQuestions',
  approval: 'approval',
  jobs: 'jobs',
  shell: 'shell',
  subagents: 'subagents',
  terminal: 'terminalController',
  permissionPresets: 'permissionPresets',
  commands: 'commands',
})

/**
 * Build the capability map for this host generation.
 * @param {object} ctx - Cordis context carrying official services.
 * @param {Record<string, unknown>} config - nano-bridge config from the runtime patch.
 * @param {{schedule?: boolean}} [facts] - startup facts not visible as plain services.
 * @returns {Record<string, {status: 'supported'|'unavailable'|'unsupported', mode?: string, detail?: string}>}
 */
export function buildCapabilities(ctx, config, facts = {}) {
  const capabilities = {}
  for (const [name, serviceKey] of Object.entries(SERVICE_CAPABILITIES)) {
    const present = ctx.get(serviceKey) !== undefined
    capabilities[name] = present
      ? { status: 'supported' }
      : { status: 'unsupported', detail: `${serviceKey} is not composed in this profile` }
  }

  // `!` also needs the per-call policy service; without it the executor cannot
  // be asked for the user-permission policy and the command must not run.
  if (ctx.get('shell') !== undefined && ctx.get('sandboxPolicy') === undefined) {
    capabilities.shell = {
      status: 'unsupported',
      detail: 'sandboxPolicy is not composed; the user-permission policy cannot be resolved',
    }
  }

  // Model selection is served either by the Web session controller or by the
  // official CLI primitives (llm registry + agentDefaultModel + selection hooks).
  const modelSelection =
    ctx.get('sessionController') !== undefined ||
    (ctx.get('llm') !== undefined && ctx.get('agentDefaultModel') !== undefined)
  capabilities.modelSelection = modelSelection
    ? { status: 'supported' }
    : { status: 'unsupported', detail: 'the official model registry is not composed' }

  if (capabilities.attachments?.status === 'supported') {
    capabilities.attachments = {
      status: 'supported',
      mode: 'images+files',
      detail: 'images are admitted at submit time; generic files use session-bound receipts',
    }
  }
  if (capabilities.fileReferences?.status === 'supported') {
    capabilities.fileReferences = {
      status: 'supported',
      detail: 'path-only @file candidates from the official provider',
    }
  }
  capabilities['tool-ask-user'] = {
    status: ctx.get('userQuestions') === undefined ? 'unsupported' : 'supported',
    mode: typeof config.askUserMode === 'string' ? config.askUserMode : 'blocking',
    detail:
      ctx.get('userQuestions') === undefined
        ? 'the tool is not mounted because userQuestions is absent'
        : 'S03 registers the answerer at host startup',
  }

  // S11 mounts the official scheduler only after its storage route and lease
  // both passed; startup fails instead of reporting a degraded schedule.
  capabilities.schedule = facts.schedule === true
    ? { status: 'supported', detail: '官方 schedule 服务已按根会话隔离挂载' }
    : { status: 'unsupported', detail: 'the schedule backend is not mounted' }
  // S10 observes the official job registry and delivers through the official
  // Agent followup/inject surface. It owns no process and no second output.
  capabilities.monitor = {
    status:
      ctx.get('jobs') !== undefined && ctx.get('agents') !== undefined
        ? 'supported'
        : 'unsupported',
    detail:
      ctx.get('jobs') !== undefined && ctx.get('agents') !== undefined
        ? 'bounded job-output observation; followup/inject delivery'
        : 'the official job registry or Agent service is not composed',
  }

  // S05 reads the official registry; when the registry row is absent the
  // catalog and preview methods report `nano/unsupported` instead of an empty
  // list that would look like "no skills installed".
  capabilities.skills =
    ctx.get('skills') === undefined
      ? { status: 'unsupported', detail: 'the official skill registry is not composed' }
      : { status: 'supported' }

  // This profile mounts no HTTP/WebServer plugin; keep a negative capability so
  // the Rust side can assert the no-listener property instead of inferring it.
  capabilities.networkListener = {
    status: 'unsupported',
    detail: 'the Nano profile opens no HTTP or WebSocket listener',
  }

  return capabilities
}
