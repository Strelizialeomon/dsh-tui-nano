// Read-only UI probe: installed dsh-tui renderer + fabricated conversation.
// No backend, model request, real session, or product-source modification.
import { createRequire, registerHooks } from 'node:module'
import { pathToFileURL } from 'node:url'
import { PassThrough, Writable } from 'node:stream'
import { writeFileSync } from 'node:fs'

process.env.NODE_ENV = 'production'
process.env.FORCE_COLOR = '3'
process.env.DSH_TUI_THEME = 'dark'
process.env.DSH_TUI_LANG = 'zh'
const pkg = '/Users/sunchongsheng/.dsh/profiles/dsh-tui/node_modules/@deepseek-harness-tui/dsh-tui'
const load = name => import(pathToFileURL(`${pkg}/lib/types/${name}.js`).href)
const req = createRequire(`${pkg}/package.json`)
// The real profile host supplies these peer packages; use its installed copy.
const hostReq = createRequire('/Users/sunchongsheng/.nvm/versions/node/v24.18.0/lib/node_modules/@deepseek-ai/dsh/package.json')
registerHooks({
  resolve(specifier, context, nextResolve) {
    try { return nextResolve(specifier, context) } catch (error) {
      if (error.code !== 'ERR_MODULE_NOT_FOUND' || !specifier.startsWith('@deepseek-ai/')) throw error
      return { url: pathToFileURL(hostReq.resolve(specifier)).href, shortCircuit: true }
    }
  },
})
await load('force-production-react')
const React = req('react')
const [{ render, AlternateScreen }, { Chat }, { QuestionStore }, commands, { default: instances }] = await Promise.all([
  load('ui'), load('screens/Chat'), load('dsh-adapter/questions'), load('commands'), load('ink/instances'),
])
const COLS = Number(process.argv[2] ?? 100)
const ROWS = 40
const sleep = ms => new Promise(resolve => setTimeout(resolve, ms))
let bytes = 0
class FakeStdout extends Writable {
  columns = COLS; rows = ROWS; isTTY = true
  _write(chunk, _encoding, done) { bytes += Buffer.byteLength(chunk); done() }
}
class FakeStderr extends Writable {
  isTTY = true
  _write(_chunk, _encoding, done) { done() }
}
class FakeStdin extends PassThrough {
  isTTY = true
  setRawMode() { return this }
  ref() { return this }
  unref() { return this }
}
const stdin = new FakeStdin(), stdout = new FakeStdout(), stderr = new FakeStderr()
const rows = []
for (let turn = 1; turn <= 120; turn++) {
  rows.push({ id: turn * 2 - 1, kind: 'user', text: `问题 ${turn}` })
  rows.push({ id: turn * 2, kind: 'assistant', text: Array.from({ length: 10 }, (_, i) => `回复 ${turn} 第 ${i + 1} 行：用于检查滚动路由的虚构内容。`).join('\n') })
}
const listeners = new Set()
const noop = () => {}
const channel = {
  whaleIdle: false, whale: false, version: 0, rows, status: 'idle', sessionTitle: 'probe', agentId: 'probe',
  model: 'deepseek-v4-flash', provider: 'deepseek', reasoningEffort: 'max', effortLevels: [],
  tokens: { input: 0, output: 0 }, cwd: '/tmp/demo', displayCwd: '/tmp/demo', gitBranch: 'main',
  working: false, spinnerMode: 'requesting', responseChars: 0, activeToolCount: 0, turnStart: 0,
  pending: [], commandList: commands.LOCAL_COMMANDS, notifications: [], mode: { plan: false, sandbox: undefined },
  activityFrames: 'moon8', agentPreset: undefined, subagents: [], lastUserText: '问题 120',
  subscribe(cb) { listeners.add(cb); return () => listeners.delete(cb) },
  submit: noop, cancel: noop, clear: noop, notify: noop,
  listModels: async () => [], listSessions: async () => [], deleteSession: async () => true,
  renameSessionTo: async () => true, setResumeTarget: noop, loadOlder: noop, mcpStatus: () => [], pushLocal: noop,
  commandCompletions: input => commands.completeCommands(input),
}
const frames = []
const app = await render(React.createElement(AlternateScreen, null, React.createElement(Chat, {
  channel, questionStore: new QuestionStore(), fullscreen: true,
})), { stdin, stdout, stderr, patchConsole: false, exitOnCtrlC: false, terminalImages: false,
  onFrame: frame => frames.push({ time: performance.now(), ...frame }),
})
function scrollNodes(node, out = []) {
  if (node.scrollViewportHeight > 0) out.push(node)
  for (const child of node.childNodes ?? []) scrollNodes(child, out)
  return out
}
const ink = instances.get(stdout)
function mainScroll() {
  return scrollNodes(ink.rootNode).sort((a, b) => b.scrollViewportHeight - a.scrollViewportHeight)[0]
}
async function gesture(label, screenRow, count, gap) {
  await sleep(450)
  const start = mainScroll()?.scrollTop
  const index = frames.length
  for (let i = 0; i < count; i++) {
    stdin.write(`\x1b[<64;${Math.floor(COLS / 2)};${screenRow}M`)
    await sleep(gap)
  }
  await sleep(250)
  return { label, screenRow, events: count, gapMs: gap, scrollTopBefore: start,
    scrollTopAfter: mainScroll()?.scrollTop,
    distance: start - mainScroll()?.scrollTop,
    frames: frames.length - index,
  }
}
try {
  await sleep(800)
  if (!mainScroll()) throw new Error('No scroll viewport mounted')
  const results = []
  results.push(await gesture('warmup', 15, 3, 200))
  results.push(await gesture('content-slow', 15, 3, 200))
  results.push(await gesture('footer-slow', 39, 3, 200))
  results.push(await gesture('content-fast', 15, 24, 8))
  results.push(await gesture('footer-fast', 39, 24, 8))
  const measured = frames.slice(5).map(f => f.durationMs).sort((a, b) => a - b)
  const result = { installedVersion: '0.15.0 with local scroll patch', columns: COLS, terminalRows: ROWS,
    fabricatedTurns: 120, results,
    renderOnly: { frames: measured.length, p50Ms: measured[Math.floor(measured.length * .5)],
      p95Ms: measured[Math.floor(measured.length * .95)], maxMs: measured.at(-1), outputBytes: bytes },
    limitations: 'Memory-backed output; no terminal emulator or compositor. Render timings exclude React work outside the onFrame interval and are not input-to-display latency.',
  }
  writeFileSync(new URL(`installed-probe-${COLS}.json`, import.meta.url), JSON.stringify(result, null, 2))
  console.log(JSON.stringify(result, null, 2))
} finally {
  await app.unmount()
}
process.exit(0)
