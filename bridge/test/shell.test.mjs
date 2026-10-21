// S08 manual shell: user-permission policy per call, synthetic bash context,
// and results that keep exit/signal/truncation facts.
import test from 'node:test'
import assert from 'node:assert/strict'

import { ShellService, syntheticText } from '../shell/service.mjs'

const ROOT = 'session-root'

function service({ result, policyCall, injectCall, shell = true, policy = true } = {}) {
  const calls = { policy: policyCall ?? (() => {}), inject: injectCall ?? (() => {}) }
  const ctx = {
    get(name) {
      if (name === 'shell') {
        return shell
          ? {
              resolve(request) {
                return { ...request, timeoutMs: 60_000, onExpiry: 'kill', stdoutMaxBytes: 65536 }
              },
              async execute(spec) {
                calls.spec = spec
                return { result: async () => result }
              },
            }
          : undefined
      }
      if (name === 'sandboxPolicy') {
        return policy
          ? {
              resolve(request) {
                calls.policy(request)
                return { mode: 'danger-full-access', workspaceRoot: '/work' }
              },
            }
          : undefined
      }
      return undefined
    },
  }
  const agent = {
    inject(message) {
      calls.inject(message)
    },
  }
  const session = { id: ROOT, header: { cwd: '/work' } }
  return {
    calls,
    shell: new ShellService({ ctx, agent, session, rootSessionId: ROOT }),
  }
}

const success = {
  exitCode: 0,
  signal: null,
  timedOut: false,
  aborted: false,
  stdout: { text: 'hello\n', truncated: false },
  stderr: { text: '', truncated: false },
}

test('exec uses a per-call danger-full-access policy and injects bash context', async () => {
  let injected
  const { shell, calls } = service({
    result: success,
    injectCall: (message) => {
      injected = message
    },
  })
  const result = await shell.exec({ address: { rootSessionId: ROOT }, command: 'printf hello' })
  assert.equal(result.exitCode, 0)
  assert.equal(result.signal, null)
  assert.equal(calls.spec.command, 'printf hello')
  assert.equal(calls.spec.workdir, '/work')
  assert.equal(calls.spec.sandboxPolicy.mode, 'danger-full-access')
  assert.equal(injected.role, 'user')
  // The synthetic context is not a user-authored prompt and never carries an
  // approval; an Agent cannot treat it as authorization.
  assert.equal(injected.source.kind, 'bash-mode')
  assert.notEqual(injected.source.kind, 'user')
  assert.match(injected.content[0].text, /^<bash-input>printf hello<\/bash-input>/)
  assert.match(injected.content[0].text, /<bash-stdout>hello\n<\/bash-stdout>/)
  assert.match(injected.content[0].text, /<bash-stderr><\/bash-stderr>/)
  assert.equal(result.injectedMessageId, injected.id)
})

test('the policy request carries the session but not a profile mutation', async () => {
  let request
  const { shell, calls } = service({
    result: success,
    policyCall: (value) => {
      request = value
    },
  })
  await shell.exec({ address: { rootSessionId: ROOT }, command: 'true' })
  assert.equal(request.mode, 'danger-full-access')
  assert.equal(request.session.id, ROOT)
  assert.equal(calls.spec.onExpiry, 'kill')
})

test('a nonzero exit is a result, not an RPC error', async () => {
  const { shell } = service({
    result: { ...success, exitCode: 3, stdout: { text: '', truncated: false } },
  })
  const result = await shell.exec({ address: { rootSessionId: ROOT }, command: 'exit 3' })
  assert.equal(result.exitCode, 3)
  assert.equal(result.timedOut, false)
})

test('a signal and a timeout keep their distinct facts', async () => {
  const { shell } = service({
    result: { ...success, exitCode: null, signal: 'SIGTERM', timedOut: true },
  })
  const result = await shell.exec({ address: { rootSessionId: ROOT }, command: 'sleep 100' })
  assert.equal(result.exitCode, null)
  assert.equal(result.signal, 'SIGTERM')
  assert.equal(result.timedOut, true)
})

test('truncation and spill paths stay visible in the injected text', async () => {
  let injected
  const { shell } = service({
    result: {
      ...success,
      stdout: { text: 'tail', truncated: true, spillPath: '/tmp/spill.txt' },
    },
    injectCall: (message) => {
      injected = message
    },
  })
  await shell.exec({ address: { rootSessionId: ROOT }, command: 'seq 1 100000' })
  assert.match(injected.content[0].text, /\[output truncated; full output: \/tmp\/spill\.txt\]/)
})

test('escape-heavy output is bounded before injection and points at the spill file', async () => {
  let injected
  const huge = '\u0001'.repeat(120_000)
  const { shell } = service({
    result: {
      ...success,
      stdout: { text: huge, truncated: true, spillPath: '/tmp/full-output.txt' },
    },
    injectCall: (message) => {
      injected = message
    },
  })
  await shell.exec({ address: { rootSessionId: ROOT }, command: 'dump' })
  assert.ok(
    Buffer.byteLength(JSON.stringify(injected), 'utf8') <= 200_000,
    'injected message stays under the event-frame budget',
  )
  assert.match(injected.content[0].text, /injected context truncated/)
  assert.match(injected.content[0].text, /\/tmp\/full-output\.txt/)
})

test('an injection failure does not lose the command result', async () => {
  const { shell } = service({
    result: success,
    injectCall: () => {
      throw new Error('inbox full')
    },
  })
  const result = await shell.exec({ address: { rootSessionId: ROOT }, command: 'true' })
  assert.equal(result.exitCode, 0)
  assert.equal(result.injectedMessageId, undefined)
  assert.match(result.injectError, /inbox full/)
})

test('an executor failure is reported as exec-failed and never as an exit', async () => {
  const ctx = {
    get(name) {
      if (name === 'shell') {
        return {
          resolve: (request) => request,
          async execute() {
            throw new Error('spawn denied')
          },
        }
      }
      if (name === 'sandboxPolicy') return { resolve: () => ({ mode: 'danger-full-access' }) }
      return undefined
    },
  }
  const shell = new ShellService({
    ctx,
    agent: { inject() {} },
    session: { header: { cwd: '/work' } },
    rootSessionId: ROOT,
  })
  await assert.rejects(
    shell.exec({ address: { rootSessionId: ROOT }, command: 'true' }),
    (error) => error.rpcName === 'nano/shell/exec-failed' && /spawn denied/.test(error.message),
  )
})

test('a missing executor or policy is unsupported, not a fake success', async () => {
  const noShell = service({ result: success, shell: false })
  await assert.rejects(
    noShell.shell.exec({ address: { rootSessionId: ROOT }, command: 'true' }),
    (error) => error.rpcName === 'nano/unsupported',
  )
  const noPolicy = service({ result: success, policy: false })
  await assert.rejects(
    noPolicy.shell.exec({ address: { rootSessionId: ROOT }, command: 'true' }),
    (error) => error.rpcName === 'nano/unsupported',
  )
})

test('an empty command is rejected before execution', async () => {
  const { shell } = service({ result: success })
  await assert.rejects(
    shell.exec({ address: { rootSessionId: ROOT }, command: '  ' }),
    (error) => error.rpcName === 'jsonrpc/invalid-params',
  )
})

test('another session address is refused', async () => {
  const { shell } = service({ result: success })
  await assert.rejects(
    shell.exec({ address: { rootSessionId: 'other' }, command: 'true' }),
    (error) => error.rpcName === 'nano/conversation/address',
  )
})

test('synthetic text always carries all three tags', () => {
  const text = syntheticText('!!echo hi', {
    stdout: { text: '!echo hi: command not found', truncated: false },
    stderr: { text: '', truncated: false },
  })
  assert.equal(
    text,
    [
      '<bash-input>!!echo hi</bash-input>',
      '<bash-stdout>!echo hi: command not found</bash-stdout>',
      '<bash-stderr></bash-stderr>',
    ].join('\n'),
  )
})
