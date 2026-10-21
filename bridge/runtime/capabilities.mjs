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
  subagents: 'subagents',
  terminal: 'terminalController',
  permissionPresets: 'permissionPresets',
  commands: 'commands',
})

/**
 * Build the capability map for this host generation.
 * @param {object} ctx - Cordis context carrying official services.
 * @param {Record<string, unknown>} config - nano-bridge config from the runtime patch.
 * @returns {Record<string, {status: 'supported'|'unavailable'|'unsupported', mode?: string, detail?: string}>}
 */
export function buildCapabilities(ctx, config) {
  const capabilities = {}
  for (const [name, serviceKey] of Object.entries(SERVICE_CAPABILITIES)) {
    const present = ctx.get(serviceKey) !== undefined
    capabilities[name] = present
      ? { status: 'supported' }
      : { status: 'unsupported', detail: `${serviceKey} is not composed in this profile` }
  }

  // Model selection is served either by the Web session controller or by the
  // official CLI primitives (llm registry + agentDefaultModel + selection hooks).
  const modelSelection =
    ctx.get('sessionController') !== undefined ||
    (ctx.get('llm') !== undefined && ctx.get('agentDefaultModel') !== undefined)
  capabilities.modelSelection = modelSelection
    ? { status: 'supported' }
    : { status: 'unsupported', detail: 'the official model registry is not composed' }

  // The seams are composed, but S01 registers no answerer. Reporting them as
  // supported would claim an interaction path that cannot answer; S03 owns the
  // single answerer for each seam.
  capabilities.approval = {
    status: 'unavailable',
    detail: 'the approval answerer belongs to S03',
  }
  capabilities.userQuestions = {
    status: 'unavailable',
    detail: 'the question answerer belongs to S03',
  }
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
    status: ctx.get('userQuestions') === undefined ? 'unsupported' : 'unavailable',
    mode: typeof config.askUserMode === 'string' ? config.askUserMode : 'blocking',
    detail: 'the tool is mounted, but no answerer is registered until S03',
  }

  // S11 and S10 own the schedule and Monitor backends; neither is composed by S01.
  capabilities.schedule = {
    status: 'unsupported',
    detail: 'the schedule backend belongs to S11',
  }
  capabilities.monitor = {
    status: 'unsupported',
    detail: 'the Monitor backend belongs to S10',
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
