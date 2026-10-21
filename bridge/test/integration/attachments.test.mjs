// Real-host S06 integration: file-reference discovery, chunked image/file
// staging, ordered prompt admission, durable references, and resume replay.
//
// Runs only with NANO_E2E=1 because it installs a profile and starts dsh.
import test from 'node:test'
import assert from 'node:assert/strict'
import fs from 'node:fs'
import os from 'node:os'
import path from 'node:path'
import { spawn, execFileSync } from 'node:child_process'
import { randomUUID } from 'node:crypto'
import { fileURLToPath } from 'node:url'

const bridgeRoot = fileURLToPath(new URL('../..', import.meta.url))
const installScript = path.join(bridgeRoot, 'profile', 'install.mjs')
const PNG = Buffer.from(
  'iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNk+M9QDwADhgGAWjR9awAAAABJRU5ErkJggg==',
  'base64',
)
const PNG_BASE64 = PNG.toString('base64')
const FORMAT_SAMPLES = [
  [
    'pixel.gif',
    'image/gif',
    Buffer.from('R0lGODlhAQABAIAAAExpcf8AACH5BAUAAAAALAAAAAABAAEAAAICTAEAOw==', 'base64'),
  ],
  [
    'pixel.webp',
    'image/webp',
    Buffer.from('UklGRjwAAABXRUJQVlA4IDAAAADQAQCdASoBAAEAAUAmJaACdLoB+AADsAD+8ut//NgVzXPv9//S4P0uD9Lg/9KQAAA=', 'base64'),
  ],
  [
    'pixel.jpg',
    'image/jpeg',
    Buffer.from('/9j/2wBDAAYEBQYFBAYGBQYHBwYIChAKCgkJChQODwwQFxQYGBcUFhYaHSUfGhsjHBYWICwgIyYnKSopGR8tMC0oMCUoKSj/2wBDAQcHBwoIChMKChMoGhYaKCgoKCgoKCgoKCgoKCgoKCgoKCgoKCgoKCgoKCgoKCgoKCgoKCgoKCgoKCgoKCgoKCj/wAARCAABAAEDASIAAhEBAxEB/8QAFQABAQAAAAAAAAAAAAAAAAAAAAf/xAAUEAEAAAAAAAAAAAAAAAAAAAAA/8QAFQEBAQAAAAAAAAAAAAAAAAAABgj/xAAUEQEAAAAAAAAAAAAAAAAAAAAA/9oADAMBAAIRAxEAPwCdABykX//Z', 'base64'),
  ],
]

function patchFor({ dataRoot, sessionId, work, runtimeId, generation, mode, nonce }) {
  const patchPath = path.join(dataRoot, 'runtime', `${runtimeId}.json`)
  fs.mkdirSync(path.dirname(patchPath), { recursive: true, mode: 0o700 })
  fs.writeFileSync(
    patchPath,
    JSON.stringify([
      { id: 'session-persistence-jsonl', config: { root: path.join(dataRoot, 'sessions') } },
      { id: 'storage-json', config: { root: path.join(dataRoot, 'runtime-data', sessionId, 'storage') } },
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
          askUserMode: 'blocking',
        },
      },
    ]),
    { mode: 0o600 },
  )
  return patchPath
}

function startHost({ dsh, dshHome, work, patchPath }) {
  const child = spawn(process.execPath, [dsh, '--profile', 'dsh-tui-nano', '--patch', patchPath], {
    cwd: work,
    env: { ...process.env, DSH_HOME: dshHome },
    stdio: ['ignore', 'pipe', 'pipe', 'pipe', 'pipe'],
  })
  let stderr = ''
  child.stderr.on('data', (chunk) => {
    stderr += chunk.toString()
  })
  const frames = []
  const waiters = []
  let buffer = Buffer.alloc(0)
  child.stdio[4].on('data', (chunk) => {
    buffer = Buffer.concat([buffer, chunk])
    while (true) {
      const index = buffer.indexOf(0x0a)
      if (index < 0) break
      const line = buffer.subarray(0, index).toString('utf8')
      buffer = buffer.subarray(index + 1)
      if (line.trim() === '') continue
      const frame = JSON.parse(line)
      if (process.env.NANO_DEBUG === '1') console.error('FRAME', JSON.stringify(frame))
      frames.push(frame)
      for (const waiter of [...waiters]) {
        if (waiter.match(frame)) {
          waiters.splice(waiters.indexOf(waiter), 1)
          clearTimeout(waiter.timer)
          waiter.resolve(frame)
        }
      }
    }
  })
  const waitFor = (match, timeout = 60_000) =>
    new Promise((resolve, reject) => {
      const existing = frames.find(match)
      if (existing !== undefined) return resolve(existing)
      const waiter = { match, resolve }
      waiter.timer = setTimeout(() => {
        waiters.splice(waiters.indexOf(waiter), 1)
        reject(
          new Error(
            `timed out waiting for frame; recent=${JSON.stringify(frames.slice(-8))}; stderr=${stderr.slice(-3000)}`,
          ),
        )
      }, timeout)
      waiters.push(waiter)
    })
  let counter = 0
  const send = (method, params) => {
    const id = `req-${++counter}`
    child.stdio[3].write(`${JSON.stringify({ jsonrpc: '2.0', id, method, params })}\n`)
    return id
  }
  const call = async (method, params) => {
    const id = send(method, params)
    return waitFor((frame) => frame.id === id)
  }
  return {
    child,
    frames,
    stderr: () => stderr,
    waitFor,
    send,
    call,
    stop: async () => {
      if (child.exitCode === null) child.kill('SIGKILL')
    },
  }
}

test(
  'attachment staging, ordered admission, discovery, and resume',
  { skip: process.env.NANO_E2E !== '1' ? 'set NANO_E2E=1' : false, timeout: 240_000 },
  async (t) => {
    const root = fs.mkdtempSync(path.join(os.tmpdir(), 'nano-attach-e2e-'))
    const dshHome = path.join(root, 'dsh-home')
    const dataRoot = path.join(root, 'data')
    const work = path.join(root, 'work')
    fs.mkdirSync(path.join(work, 'src'), { recursive: true })
    fs.writeFileSync(path.join(work, 'src', 'main.rs'), 'fn main() {}\n')
    fs.writeFileSync(path.join(work, 'src', 'lib.rs'), 'pub fn f() {}\n')
    fs.writeFileSync(path.join(work, 'notes.txt'), 'hello file\n')
    fs.writeFileSync(path.join(work, 'empty.log'), '')
    fs.writeFileSync(path.join(work, 'pixel.png'), PNG)
    const dsh = execFileSync('which', ['dsh'], { encoding: 'utf8' }).trim()
    const install = execFileSync(
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
        dsh,
        '--json',
      ],
      { encoding: 'utf8' },
    )
    assert.equal(JSON.parse(install.trim()).installed, true)

    const sessionId = `session-${randomUUID()}`
    const address = { rootSessionId: sessionId }
    const firstPatch = patchFor({
      dataRoot,
      sessionId,
      work,
      runtimeId: `rt-attach-${randomUUID()}`,
      generation: 1,
      mode: 'create',
      nonce: 'attach-e2e-nonce',
    })
    const host = startHost({ dsh, dshHome, work, patchPath: firstPatch })
    t.after(() => host.stop())

    const hello = await host.waitFor(
      (frame) => frame.method === 'nano.hello' || frame.method === 'nano.startupError',
    )
    assert.equal(hello.method, 'nano.hello', `startup error: ${JSON.stringify(hello)}`)
    assert.equal(hello.params.capabilities.attachments.status, 'supported')
    assert.equal(hello.params.capabilities.fileReferences.status, 'supported')
    assert.equal(hello.params.attachmentLimits.mediaTypes.includes('image/png'), true)
    assert.ok(hello.params.attachmentLimits.maxImageBytes > 0)
    const subscribed = await host.call('nano.subscribe', {
      address,
      kinds: ['session.event', 'conversation.status', 'conversation.inbox', 'conversation.stream'],
      fromSeq: 0,
    })
    assert.ok(subscribed.result.subscriptionId)

    // Path-only discovery returns workspace candidates and never file bytes.
    const referenced = await host.call('nano.fileReferences.list', { address, query: 'mai' })
    assert.ok(Array.isArray(referenced.result.candidates))
    assert.ok(
      referenced.result.candidates.some((candidate) => candidate.path.includes('main.rs')),
      JSON.stringify(referenced.result),
    )
    const empty = await host.call('nano.fileReferences.list', { address, query: '' })
    assert.ok(empty.result.candidates.length > 0)
    const directory = await host.call('nano.fileReferences.list', {
      address,
      query: 'src/',
    })
    assert.ok(
      directory.result.candidates.some((candidate) => candidate.path.includes('main.rs')),
      JSON.stringify(directory.result),
    )

    // Image: chunked staging, then official admission at submit time.
    const imageReceipt = `img-${randomUUID()}`
    const beginImage = await host.call('nano.attachment.begin', {
      address,
      receiptId: imageReceipt,
      kind: 'image',
      mediaType: 'image/png',
      name: 'pixel.png',
      bytes: PNG.length,
    })
    assert.equal(beginImage.result.kind, 'image')
    const split = Math.ceil(PNG.length / 2)
    for (const [index, chunk] of [PNG.subarray(0, split), PNG.subarray(split)].entries()) {
      const reply = await host.call('nano.attachment.chunk', {
        receiptId: imageReceipt,
        sequence: index,
        bytes: chunk.toString('base64'),
      })
      assert.equal(reply.result.nextSequence, index + 1)
    }
    const finishedImage = await host.call('nano.attachment.finish', { receiptId: imageReceipt })
    assert.equal(finishedImage.result.ready, true)
    assert.equal(finishedImage.result.kind, 'image')

    // A second image keeps user order; identical bytes may deduplicate in the
    // official store, which is allowed.
    const secondImageReceipt = `img2-${randomUUID()}`
    await host.call('nano.attachment.begin', {
      address,
      receiptId: secondImageReceipt,
      kind: 'image',
      mediaType: 'image/png',
      name: 'pixel-copy.png',
      bytes: PNG.length,
    })
    await host.call('nano.attachment.chunk', {
      receiptId: secondImageReceipt,
      sequence: 0,
      bytes: PNG.toString('base64'),
    })
    const finishedSecond = await host.call('nano.attachment.finish', {
      receiptId: secondImageReceipt,
    })
    assert.equal(finishedSecond.result.ready, true)

    // Generic file: exact bytes, session-bound durable receipt.
    const fileBytes = fs.readFileSync(path.join(work, 'notes.txt'))
    const fileReceipt = `file-${randomUUID()}`
    await host.call('nano.attachment.begin', {
      address,
      receiptId: fileReceipt,
      kind: 'file',
      name: 'notes.txt',
      bytes: fileBytes.length,
    })
    await host.call('nano.attachment.chunk', {
      receiptId: fileReceipt,
      sequence: 0,
      bytes: fileBytes.toString('base64'),
    })
    const finishedFile = await host.call('nano.attachment.finish', { receiptId: fileReceipt })
    assert.equal(finishedFile.result.ready, true)
    assert.equal(finishedFile.result.kind, 'file')
    assert.equal(finishedFile.result.file.name, 'notes.txt')
    assert.equal(finishedFile.result.file.bytes, fileBytes.length)
    assert.match(finishedFile.result.file.attachmentId, /^sha256:/)
    assert.doesNotMatch(JSON.stringify(finishedFile), new RegExp(work.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')))

    // A zero-byte generic file is valid and keeps its exact zero length.
    const emptyReceipt = `empty-${randomUUID()}`
    await host.call('nano.attachment.begin', {
      address,
      receiptId: emptyReceipt,
      kind: 'file',
      name: 'empty.log',
      bytes: 0,
    })
    const finishedEmpty = await host.call('nano.attachment.finish', { receiptId: emptyReceipt })
    assert.equal(finishedEmpty.result.ready, true)
    assert.equal(finishedEmpty.result.file.bytes, 0)

    const requestId = `prompt-${randomUUID()}`
    const submitted = await host.call('nano.conversation.submit', {
      address,
      requestId,
      delivery: 'queue',
      content: [
        { type: 'text', text: 'look ' },
        { type: 'image', receiptId: imageReceipt },
        { type: 'image', receiptId: secondImageReceipt },
        { type: 'file', receiptId: fileReceipt },
        { type: 'text', text: ' end' },
      ],
    })
    assert.equal(submitted.result.accepted, true)
    assert.equal(submitted.result.duplicate, false)

    const durable = await host.waitFor(
      (frame) =>
        frame.method === 'nano.event' &&
        frame.params.kind === 'session.event' &&
        frame.params.payload.type === 'agent/inbox/spliced' &&
        frame.params.payload.data?.inserted?.some(
          (message) => message.id === submitted.result.messageId,
        ),
    )
    const inserted = durable.params.payload.data.inserted.find(
      (message) => message.id === submitted.result.messageId,
    )
    assert.equal(inserted.source.rpcId, requestId)
    assert.deepEqual(
      inserted.content.map((block) => block.type),
      ['text', 'image', 'image', 'file', 'text'],
    )
    const imageBlock = inserted.content[1]
    const secondImageBlock = inserted.content[2]
    const fileBlock = inserted.content[3]
    assert.equal(secondImageBlock.attachment.mediaType, 'image/png')
    assert.equal(imageBlock.attachment.mediaType, 'image/png')
    assert.equal(imageBlock.attachment.width, 1)
    assert.equal(typeof imageBlock.attachment.attachmentId, 'string')
    assert.equal(fileBlock.attachment.name, 'notes.txt')
    assert.equal(fileBlock.attachment.bytes, fileBytes.length)
    assert.equal(Object.hasOwn(imageBlock.attachment, 'data'), false)
    const serialized = JSON.stringify(durable)
    assert.equal(serialized.includes(PNG_BASE64), false)
    assert.equal(serialized.includes(work), false)

    // Every supported source format is admitted by the official service.
    for (const [name, mediaType, buffer] of FORMAT_SAMPLES) {
      const receiptId = `${name}-${randomUUID()}`
      await host.call('nano.attachment.begin', {
        address,
        receiptId,
        kind: 'image',
        mediaType,
        name,
        bytes: buffer.length,
      })
      await host.call('nano.attachment.chunk', {
        receiptId,
        sequence: 0,
        bytes: buffer.toString('base64'),
      })
      const finished = await host.call('nano.attachment.finish', { receiptId })
      assert.equal(finished.result.ready, true, `${mediaType} did not finish`)
      const formatRequestId = `prompt-${randomUUID()}`
      const reply = await host.call('nano.conversation.submit', {
        address,
        requestId: formatRequestId,
        delivery: 'queue',
        content: [{ type: 'image', receiptId }, { type: 'text', text: ` ${name}` }],
      })
      assert.ok(reply.result?.accepted, JSON.stringify(reply))
      const event = await host.waitFor(
        (frame) =>
          frame.method === 'nano.event' &&
          frame.params.kind === 'session.event' &&
          frame.params.payload.type === 'agent/inbox/spliced' &&
          frame.params.payload.data?.inserted?.some(
            (message) => message.id === reply.result.messageId,
          ),
      )
      const message = event.params.payload.data.inserted.find(
        (item) => item.id === reply.result.messageId,
      )
      assert.equal(message.content[0].type, 'image')
      // The durable media type is the official normalized type; a GIF source
      // may become WebP/JPEG after normalization. The source format itself was
      // admitted (finish.ready above).
      assert.ok(
        ['image/png', 'image/jpeg', 'image/webp'].includes(
          message.content[0].attachment.mediaType,
        ),
        `${mediaType} normalized to ${message.content[0].attachment.mediaType}`,
      )
    }

    // Same requestId is a duplicate; the consumed receipt cannot be reused.
    const duplicate = await host.call('nano.conversation.submit', {
      address,
      requestId,
      delivery: 'queue',
      content: [{ type: 'text', text: 'different copy' }],
    })
    assert.equal(duplicate.result.duplicate, true)
    assert.equal(duplicate.result.messageId, submitted.result.messageId)
    const reused = await host.call('nano.conversation.submit', {
      address,
      requestId: `prompt-${randomUUID()}`,
      delivery: 'queue',
      content: [{ type: 'image', receiptId: imageReceipt }],
    })
    assert.ok(
      reused.error?.code === 1019 || reused.error?.code === 1020,
      JSON.stringify(reused),
    )

    const shutdown = await host.call('nano.shutdown', {})
    assert.equal(shutdown.result.clean, true)
    const exitCode = await new Promise((resolve) => {
      if (host.child.exitCode !== null) return resolve(host.child.exitCode)
      const timer = setTimeout(() => resolve('timeout'), 15_000)
      host.child.once('exit', (code) => {
        clearTimeout(timer)
        resolve(code)
      })
    })
    assert.equal(exitCode, 0, `host did not exit cleanly; stderr=${host.stderr().slice(-2000)}`)

    // Resume replay keeps the durable image/file references, not bytes.
    const resumedPatch = patchFor({
      dataRoot,
      sessionId,
      work,
      runtimeId: `rt-resume-${randomUUID()}`,
      generation: 2,
      mode: 'resume',
      nonce: 'attach-resume-nonce',
    })
    const resumed = startHost({ dsh, dshHome, work, patchPath: resumedPatch })
    t.after(() => resumed.stop())
    const resumedHello = await resumed.waitFor(
      (frame) => frame.method === 'nano.hello' || frame.method === 'nano.startupError',
    )
    assert.equal(resumedHello.method, 'nano.hello', JSON.stringify(resumedHello))
    assert.equal(resumedHello.params.session.mode, 'resumed')
    await resumed.call('nano.subscribe', { address, fromSeq: 0 })
    const replayed = await resumed.waitFor(
      (frame) =>
        frame.method === 'nano.event' &&
        frame.params.kind === 'session.event' &&
        frame.params.payload.type === 'agent/inbox/spliced' &&
        frame.params.payload.data?.inserted?.some(
          (message) => message.id === submitted.result.messageId,
        ),
    )
    const replayedFrame = JSON.stringify(replayed)
    assert.equal(replayedFrame.includes(PNG_BASE64), false)
    assert.equal(replayedFrame.includes(work), false)
    const replayedShutdown = await resumed.call('nano.shutdown', {})
    assert.equal(replayedShutdown.result.clean, true)
    t.after(() => fs.rmSync(root, { recursive: true, force: true }))
  },
)
