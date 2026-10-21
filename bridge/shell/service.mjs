// Manual shell command (`!`) host side.
//
// `!command` is a human command, not an Agent tool and not a job. It calls the
// official shell seam directly with a per-call `danger-full-access` policy so
// the command runs with the user's OS permissions. The output is injected into
// the session as a synthetic user-role context message (`bash-mode` source)
// without waking the driver. It never allocates a job id and therefore never
// appears in the model's `job_list`.
import { randomUUID } from 'node:crypto'
import { LIMITS } from '../../protocol/index.mjs'
import { RpcError } from '../runtime/errors.mjs'

// Keep the injected synthetic message below the per-event frame bound so a
// command with escape-heavy output cannot close the session subscription. The
// full result still goes to the Rust window; only the model-facing context is
// bounded, with an explicit truncation marker and spill path.
const INJECTION_MAX_JSON_BYTES = 200_000

// Manual commands share the profile executor deadline. Setting it on the
// request keeps the Rust wait bound (75s) independent of another executor's
// default (bash-local defaults to 120s).
const SHELL_TIMEOUT_MS = 60_000

/**
 * One manual-shell service per host generation.
 */
export class ShellService {
  /**
   * @param {{
   *   ctx: object,
   *   agent: object,
   *   session: object,
   *   rootSessionId: string,
   * }} options - host wiring.
   */
  constructor({ ctx, agent, session, rootSessionId }) {
    this.ctx = ctx
    this.agent = agent
    this.session = session
    this.rootSessionId = rootSessionId
    this.address = { rootSessionId }
    this.closed = false
  }

  /** Stop accepting work during teardown. */
  close() {
    this.closed = true
  }

  /**
   * Execute one `!command` in a fresh shell, bounded by the executor deadline.
   * A nonzero exit, a signal, or a timeout is a result, not an RPC error; only
   * an execution that never produced a process is reported as a failure.
   * @param {object} params - `address` and `command`.
   * @returns {Promise<object>} shell result plus the injected message id.
   */
  async exec(params) {
    if (this.closed) throw new RpcError('nano/closed', 'host is shutting down')
    this.assertAddress(params?.address)
    const command = params.command
    if (typeof command !== 'string' || command.trim() === '') {
      throw new RpcError('jsonrpc/invalid-params', 'command must be a non-empty string')
    }
    const bytes = Buffer.byteLength(command, 'utf8')
    if (bytes > LIMITS.maxSubmitTextBytes) {
      throw new RpcError(
        'jsonrpc/invalid-params',
        `command is ${String(bytes)} bytes, over the ${String(LIMITS.maxSubmitTextBytes)} byte limit`,
      )
    }
    const shell = this.ctx.get('shell')
    if (shell === undefined) {
      throw new RpcError('nano/unsupported', 'the official shell executor is not composed')
    }
    const policy = this.ctx.get('sandboxPolicy')
    if (policy === undefined) {
      throw new RpcError('nano/unsupported', 'the official sandbox policy service is not composed')
    }

    let sandboxPolicy
    try {
      // Per call only. The profile default and the Agent's sandbox stay as they
      // are; this command is the user acting directly, not an Agent escalation.
      // This is the exact override the spec names.
      sandboxPolicy = policy.resolve({ mode: 'danger-full-access' })
    } catch (error) {
      throw new RpcError('nano/shell/exec-failed', `could not resolve the user shell policy: ${describe(error)}`)
    }

    let result
    try {
      const workdir = this.session.header?.cwd ?? process.cwd()
      const spec = shell.resolve({
        command,
        workdir,
        timeoutMs: SHELL_TIMEOUT_MS,
        sandboxPolicy,
      })
      const execution = await shell.execute(spec)
      result = await execution.result()
    } catch (error) {
      throw new RpcError('nano/shell/exec-failed', `the command could not run: ${describe(error)}`)
    }

    const payload = {
      exitCode: Number.isInteger(result.exitCode) ? result.exitCode : null,
      signal: typeof result.signal === 'string' ? result.signal : null,
      timedOut: result.timedOut === true,
      aborted: result.aborted === true,
      stdout: collected(result.stdout),
      stderr: collected(result.stderr),
      // The resolved request policy, for display and tests. The executor's own
      // sandbox facts (when present) remain authoritative for enforcement.
      policy: { mode: sandboxPolicy.mode },
    }
    if (result.sandbox !== undefined) {
      payload.sandbox = {
        mode: result.sandbox.mode,
        denied: result.sandbox.denied === true,
        ...(result.sandbox.enforcement === undefined ? {} : { enforcement: result.sandbox.enforcement }),
        ...(result.sandbox.runnerFailed === true ? { runnerFailed: true } : {}),
      }
    }

    // `!!` is ordinary shell text with no context-injection path. Everything
    // else is injected best-effort: the command already ran, so a failure here
    // must not turn a real result into an infrastructure failure.
    if (params.inject !== false) {
      try {
        const messageId = randomUUID()
        const message = Object.freeze({
          id: messageId,
          role: 'user',
          content: Object.freeze([
            Object.freeze({ type: 'text', text: boundedSyntheticText(command, payload) }),
          ]),
          source: Object.freeze({ kind: 'bash-mode' }),
        })
        this.agent.inject(message)
        payload.injectedMessageId = messageId
      } catch (error) {
        payload.injectError = describe(error)
      }
    }
    return payload
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

/**
 * Build the synthetic user context exactly in the Claude Code bash-tag form.
 * Truncation and spill facts stay visible instead of silently presenting the
 * tail as the whole output.
 * @param {string} command - exact command text after `!`.
 * @param {object} result - shell result payload.
 * @param {{stdout?: number, stderr?: number}} [budgets] - optional per-stream byte caps for injection.
 * @returns {string} one text block.
 */
export function syntheticText(command, result, budgets = {}) {
  return [
    `<bash-input>${command}</bash-input>`,
    `<bash-stdout>${renderedOutput(result.stdout, budgets.stdout)}</bash-stdout>`,
    `<bash-stderr>${renderedOutput(result.stderr, budgets.stderr)}</bash-stderr>`,
  ].join('\n')
}

/** Build the injected text under the event-frame budget, shrinking streams if needed. */
function boundedSyntheticText(command, payload) {
  let budgets = {}
  let text = syntheticText(command, payload, budgets)
  for (let attempt = 0; attempt < 8; attempt += 1) {
    if (messageBytes(text) <= INJECTION_MAX_JSON_BYTES) return text
    budgets = {
      stdout: nextBudget(budgets.stdout, payload.stdout.text),
      stderr: nextBudget(budgets.stderr, payload.stderr.text),
    }
    text = syntheticText(command, payload, budgets)
  }
  // Pathological escaping can still be too large; keep the fact visible
  // instead of risking a closed subscription.
  return syntheticText(
    truncatedText(command, 4096),
    {
      stdout: { text: '[output omitted: too large to inject]', truncated: false },
      stderr: { text: '', truncated: false },
    },
  )
}

function messageBytes(text) {
  return Buffer.byteLength(
    JSON.stringify({ id: 'x', role: 'user', content: [{ type: 'text', text }], source: { kind: 'bash-mode' } }),
    'utf8',
  )
}

function nextBudget(current, text) {
  const full = Buffer.byteLength(typeof text === 'string' ? text : '', 'utf8')
  if (current === undefined) return Math.max(1024, Math.floor(full / 2))
  return Math.max(1024, Math.floor(current / 2))
}

/** Render one collected stream with its official truncation notice. */
function renderedOutput(output, budget) {
  if (typeof budget === 'number') return boundedOutput(output, budget)
  if (output.truncated !== true) return output.text
  if (typeof output.spillPath === 'string' && output.spillPath !== '') {
    return `${output.text}\n[output truncated; full output: ${output.spillPath}]`
  }
  return `${output.text}\n[output truncated]`
}

/** Keep the tail of one stream and mark the injected-only truncation. */
function boundedOutput(output, budget) {
  if (Buffer.byteLength(output.text, 'utf8') <= budget) return renderedOutput(output)
  const text = tailUtf8(output.text, budget)
  const marker =
    typeof output.spillPath === 'string' && output.spillPath !== ''
      ? `[injected context truncated; full output: ${output.spillPath}]`
      : '[injected context truncated]'
  return `${text}\n${marker}`
}

function tailUtf8(text, maxBytes) {
  const buffer = Buffer.from(text, 'utf8')
  let start = Math.max(0, buffer.length - maxBytes)
  while (start < buffer.length && (buffer[start] & 0xc0) === 0x80) start += 1
  return buffer.subarray(start).toString('utf8')
}

function truncatedText(text, maxChars) {
  const characters = Array.from(text)
  return characters.length <= maxChars ? text : `${characters.slice(0, maxChars).join('')}…`
}

/** Copy one CollectedOutput into the wire shape. */
function collected(output) {
  const value = {
    text: typeof output?.text === 'string' ? output.text : '',
    truncated: output?.truncated === true,
  }
  if (typeof output?.spillPath === 'string' && output.spillPath !== '') value.spillPath = output.spillPath
  return value
}

function describe(error) {
  return error instanceof Error ? error.message : String(error)
}
