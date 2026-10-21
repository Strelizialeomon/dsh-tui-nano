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
  capabilities['tool-ask-user'] = {
    status: ctx.get('userQuestions') === undefined ? 'unsupported' : 'unavailable',
    mode: typeof config.askUserMode === 'string' ? config.askUserMode : 'blocking',
    detail: 'the tool is mounted, but no answerer is registered until S03',
  }

  // S11 mounts the official scheduler only after its storage route and lease
  // both passed; startup fails instead of reporting a degraded schedule.
  capabilities.schedule = facts.schedule === true
    ? { status: 'supported', detail: '官方 schedule 服务已按根会话隔离挂载' }
    : { status: 'unsupported', detail: 'the schedule backend is not mounted' }
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
