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
  userQuestions: 'userQuestions',
  approval: 'approval',
  jobs: 'jobs',
  subagents: 'subagents',
  terminal: 'terminalController',
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

  capabilities['tool-ask-user'] = {
    status: ctx.get('userQuestions') === undefined ? 'unsupported' : 'supported',
    mode: typeof config.askUserMode === 'string' ? config.askUserMode : 'blocking',
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

  // This profile mounts no HTTP/WebServer plugin; keep a negative capability so
  // the Rust side can assert the no-listener property instead of inferring it.
  capabilities.networkListener = {
    status: 'unsupported',
    detail: 'the Nano profile opens no HTTP or WebSocket listener',
  }

  return capabilities
}
