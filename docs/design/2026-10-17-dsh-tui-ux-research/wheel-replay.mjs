// Deterministic replay of the existing pi-plugin helper, without changing it.
// performance.now is replaced only inside this short-lived probe process.
import { pathToFileURL } from 'node:url'
import { writeFileSync } from 'node:fs'

const helper = '/Users/sunchongsheng/code/pi-plugins/extensions/dsh-scroll-speed/assets/scroll-wheel-speed.js'
let now = 0
Object.defineProperty(globalThis, 'performance', { value: { now: () => now }, configurable: true })
for (const key of ['DSH_TUI_SCROLL_SLOW', 'DSH_TUI_SCROLL_FAST', 'DSH_TUI_SCROLL_ACCEL', 'DSH_TUI_SCROLL_DEBUG']) {
  delete process.env[key]
}
let id = 0
const results = []
async function run(label, events) {
  const { wheelStep } = await import(`${pathToFileURL(helper).href}?case=${id++}`)
  now = 0
  const rows = events.map(([time, direction]) => {
    now = time
    return wheelStep(direction)
  })
  results.push({ label, events: events.length, rows, total: rows.reduce((sum, row) => sum + row, 0) })
}
await run('slow: 3 events 200ms apart', Array.from({ length: 3 }, (_, i) => [i * 200, 1]))
await run('fast: 15 events 25ms apart', Array.from({ length: 15 }, (_, i) => [i * 25, 1]))
await run('steady burst: 24 events 8ms apart', Array.from({ length: 24 }, (_, i) => [i * 8, 1]))
await run('same 24 events delivered in one batch', Array.from({ length: 24 }, () => [0, 1]))
await run('reverse immediately after 24 fast events', [
  ...Array.from({ length: 24 }, (_, i) => [i * 8, 1]), [192, -1], [200, -1], [208, -1],
])
await run('fresh reverse without preceding gesture', [[192, -1], [200, -1], [208, -1]])
const output = results.map(row => JSON.stringify(row)).join('\n') + '\n'
writeFileSync(new URL('wheel-replay.jsonl', import.meta.url), output)
console.log(output.trimEnd())
