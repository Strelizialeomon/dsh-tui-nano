// Shared real-host harness for the S13 cross-feature acceptance.
//
// Every test runs against a synthetic HOME/DSH_HOME/data root. Nothing under
// the user's real home is read or written. Set NANO_BRIDGE_PACKAGE to exercise
// an installed payload instead of the repository bridge.
import fs from 'node:fs'
import os from 'node:os'
import path from 'node:path'
import { spawn, execFileSync } from 'node:child_process'
import { zstdDecompressSync } from 'node:zlib'
import { randomUUID } from 'node:crypto'
import { fileURLToPath } from 'node:url'

export const repoRoot = fileURLToPath(new URL('../..', import.meta.url))
export const bridgeRoot = process.env.NANO_BRIDGE_PACKAGE ?? path.join(repoRoot, 'bridge')
export const installScript = path.join(bridgeRoot, 'profile', 'install.mjs')
export const DSH = execFileSync('which', ['dsh'], { encoding: 'utf8' }).trim()

export function tempRoot(prefix = 'nano-s13-e2e-') {
  return fs.mkdtempSync(path.join(os.tmpdir(), prefix))
}

export function installProfile({ dshHome, dataRoot }) {
  const output = execFileSync(
    process.execPath,
    [
      installScript,
      '--dsh-home',
      dshHome,
      '--data-root',
      dataRoot,
      '--bridge-package',
      bridgeRoot,
      '--dsh-bin',
      DSH,
      '--json',
    ],
    { encoding: 'utf8' },
  )
  const summary = JSON.parse(output.trim().split('\n').at(-1))
  if (summary.installed !== true) throw new Error(`profile install did not run: ${output}`)
  return summary
}

export function writePatch({
  root,
  dataRoot,
  sessionId,
  runtimeId,
  work,
  mode = 'create',
  generation = 1,
  nonce = `nonce-${randomUUID()}`,
  askUserMode = 'blocking',
  route = 'nano-schedule-json',
  provider = 'deepseek-official',
  model = 'deepseek-flash',
}) {
  const patchPath = path.join(root, `patch-${generation}-${randomUUID()}.json`)
  fs.mkdirSync(path.dirname(patchPath), { recursive: true, mode: 0o700 })
  fs.writeFileSync(
    patchPath,
    JSON.stringify([
      { id: 'session-persistence-jsonl', config: { root: path.join(dataRoot, 'sessions') } },
      {
        id: 'storage-json',
        config: { root: path.join(dataRoot, 'runtime-data', sessionId, 'storage') },
      },
      {
        id: 'storage-domain',
        config: {
          backend: 'json',
          routes: { schedule: route, nano_probe: route },
        },
      },
      {
        id: 'nano-bridge',
        config: {
          runtimeId,
          generation,
          rootSessionId: sessionId,
          cwd: work,
          dataRoot,
          sessionMode: mode,
          officialVersion: '0.2.1-rc.2',
          nonce,
          askUserMode,
          provider,
          model,
        },
      },
    ]),
    { mode: 0o600 },
  )
  return patchPath
}

export function startHost({ dshHome, patchPath, work, env = {} }) {
  const child = spawn(process.execPath, [DSH, '--profile', 'dsh-tui-nano', '--patch', patchPath], {
    cwd: work,
    env: { ...process.env, DSH_HOME: dshHome, ...env },
    stdio: ['ignore', 'pipe', 'pipe', 'pipe', 'pipe'],
  })
  const state = {
    child,
    frames: [],
    waiters: [],
    stderr: '',
    counter: 0,
    buffer: Buffer.alloc(0),
    inputClosed: false,
  }
  child.stdio[3].on('error', () => {
    state.inputClosed = true
  })
  child.stderr.on('data', (chunk) => {
    state.stderr += chunk.toString()
  })
  child.stdio[4].on('data', (chunk) => {
    state.buffer = Buffer.concat([state.buffer, chunk])
    while (true) {
      const index = state.buffer.indexOf(0x0a)
      if (index < 0) break
      const line = state.buffer.subarray(0, index).toString('utf8')
      state.buffer = state.buffer.subarray(index + 1)
      if (line.trim() === '') continue
      let frame
      try {
        frame = JSON.parse(line)
      } catch {
        continue
      }
      state.frames.push(frame)
      for (const waiter of [...state.waiters]) {
        if (waiter.match(frame)) {
          state.waiters.splice(state.waiters.indexOf(waiter), 1)
          clearTimeout(waiter.timer)
          waiter.resolve(frame)
        }
      }
    }
  })
  state.call = async (method, params, timeout = 60_000) => {
    const id = `req-${++state.counter}`
    if (state.inputClosed || child.exitCode !== null) {
      return Promise.reject(new Error('host input is closed'))
    }
    child.stdio[3].write(`${JSON.stringify({ jsonrpc: '2.0', id, method, params })}\n`)
    return state.waitFor((frame) => frame.id === id, timeout)
  }
  state.waitFor = (match, timeout = 60_000) =>
    new Promise((resolve, reject) => {
      const existing = state.frames.find(match)
      if (existing !== undefined) return resolve(existing)
      const waiter = { match, resolve }
      waiter.timer = setTimeout(() => {
        state.waiters.splice(state.waiters.indexOf(waiter), 1)
        reject(
          new Error(
            `timed out waiting for a frame; recent=${JSON.stringify(state.frames.slice(-8))}; stderr=${state.stderr.slice(-2000)}`,
          ),
        )
      }, timeout)
      state.waiters.push(waiter)
    })
  state.stop = async () => {
    if (child.exitCode !== null) return child.exitCode
    let reply
    if (!state.inputClosed && child.exitCode === null) {
      reply = await Promise.race([
        state.call('nano.shutdown', {}, 20_000).catch(() => undefined),
        new Promise((resolve) => {
          if (child.exitCode !== null) return resolve(undefined)
          child.once('exit', () => resolve(undefined))
        }),
      ])
    }
    const clean = reply?.result?.clean
    const code = await new Promise((resolve) => {
      if (child.exitCode !== null) return resolve(child.exitCode)
      const timer = setTimeout(() => {
        child.kill('SIGKILL')
        resolve('timeout')
      }, 15_000)
      child.once('exit', (value) => {
        clearTimeout(timer)
        resolve(value)
      })
    })
    return { code, clean, reply }
  }
  return state
}

export async function boot(options) {
  const host = startHost(options)
  const hello = await host.waitFor(
    (frame) => frame.method === 'nano.hello' || frame.method === 'nano.startupError',
    90_000,
  )
  return { host, hello }
}

export function address(sessionId) {
  return { rootSessionId: sessionId }
}

export function sessionLogs(dataRoot) {
  const root = path.join(dataRoot, 'sessions')
  const files = []
  const walk = (directory) => {
    if (!fs.existsSync(directory)) return
    for (const entry of fs.readdirSync(directory, { withFileTypes: true })) {
      const child = path.join(directory, entry.name)
      if (entry.isDirectory()) walk(child)
      else if (entry.name.endsWith('.jsonl') || entry.name.endsWith('.jsonl.zstd')) files.push(child)
    }
  }
  walk(root)
  return files
}

function readLog(file) {
  if (!file.endsWith('.zstd')) return fs.readFileSync(file, 'utf8')
  // The official writer appends multiple zstd frames; Node's zstd API stops
  // after the first frame, so prefer the CLI and fall back to Node.
  try {
    return execFileSync('zstd', ['-dc', file], { encoding: 'utf8', maxBuffer: 64 * 1024 * 1024 })
  } catch {
    return zstdDecompressSync(fs.readFileSync(file)).toString('utf8')
  }
}

export function fileContains(file, marker) {
  return readLog(file).includes(marker)
}

export function sleep(ms) {
  return new Promise((resolve) => setTimeout(resolve, ms))
}
