// Attachment host unit tests: chunk order, session-bound receipts, official
// prompt admission, image-model refusal, and receipt retirement.
import test from 'node:test'
import assert from 'node:assert/strict'
import fs from 'node:fs'
import os from 'node:os'
import path from 'node:path'

import { AttachmentService } from '../attachments/service.mjs'
import { ConversationService } from '../conversation/service.mjs'

const ROOT = 'session-root'
const ADDRESS = { rootSessionId: ROOT }

function attachmentError(code, message = code) {
  const error = new Error(message)
  error.code = code
  return error
}

function fakeStore({ imageLimits, failAdmission, files = true, admissionDelayMs = 0 } = {}) {
  const store = {
    imageLimits: imageLimits ?? {
      maxImageBytes: 20 * 1024 * 1024,
      maxImagesPerMessage: 20,
      maxMessageImageBytes: 200 * 1024 * 1024,
      maxImagePixels: 64_000_000,
      maxImageDimension: 8192,
      mediaTypes: ['image/png', 'image/jpeg', 'image/webp', 'image/gif'],
    },
    admitted: [],
    saved: [],
    files,
    async admitPromptContent(content) {
      if (admissionDelayMs > 0) {
        await new Promise((resolve) => setTimeout(resolve, admissionDelayMs))
      }
      store.admitted.push(content)
      if (failAdmission !== undefined) throw failAdmission
      const images = content.filter((part) => part.type === 'image')
      let imageIndex = 0
      return content.map((part) => {
        if (part.type !== 'image') return part
        imageIndex += 1
        return {
          type: 'image',
          attachment: {
            attachmentId: `image-ref-${imageIndex}`,
            mediaType: part.mediaType,
            bytes: Buffer.from(part.data, 'base64').length,
            width: 1,
            height: 1,
          },
        }
      })
    },
    async saveFileStream({ data, name }) {
      if (!files) throw attachmentError('ATTACHMENT_FILES_UNSUPPORTED')
      const chunks = []
      for await (const chunk of data) chunks.push(Buffer.from(chunk))
      const bytes = Buffer.concat(chunks)
      const ref = { attachmentId: 'file-ref-1', name: name ?? 'file', bytes: bytes.length }
      store.saved.push({ ref, bytes })
      return ref
    },
    isAttachmentError(error) {
      return typeof error?.code === 'string' && error.code === error.code.toUpperCase()
    },
  }
  return store
}

function fakeContext({ store = fakeStore(), refs, llm } = {}) {
  const services = new Map()
  if (store !== undefined) services.set('attachments', store)
  if (refs !== undefined) services.set('fileReferences', refs)
  if (llm !== undefined) services.set('llm', llm)
  return {
    get: (name) => services.get(name),
    on() {
      return () => {}
    },
  }
}

function fakeAgent() {
  return { id: ROOT, status: 'running' }
}

function fakeSession() {
  return {
    seq: 1,
    requestHeader: () => undefined,
    snapshotEvents() {
      return []
    },
  }
}

function serviceFor(options = {}) {
  const dataRoot = fs.mkdtempSync(path.join(os.tmpdir(), 'nano-attachments-'))
  const store = options.store ?? fakeStore(options)
  const ctx = fakeContext({ store, refs: options.refs, llm: options.llm })
  const service = new AttachmentService({
    ctx,
    agent: fakeAgent(),
    session: fakeSession(),
    rootSessionId: ROOT,
    dataRoot,
    config: options.config ?? {},
  })
  return { service, store, dataRoot, ctx }
}

function beginImage(service, receiptId = 'r-img', overrides = {}) {
  return service.begin({
    address: ADDRESS,
    receiptId,
    kind: 'image',
    mediaType: 'image/png',
    name: 'pixel.png',
    ...overrides,
  })
}

test('image chunks stage in order and admission produces a durable image block', async () => {
  const { service, store } = serviceFor()
  beginImage(service)
  assert.deepEqual(service.chunk({ receiptId: 'r-img', sequence: 0, bytes: 'AAE=' }), {
    receiptId: 'r-img',
    received: 2,
    nextSequence: 1,
  })
  assert.equal(service.chunk({ receiptId: 'r-img', sequence: 1, bytes: 'Aw==' }).received, 3)
  const finished = await service.finish({ receiptId: 'r-img' })
  assert.equal(finished.ready, true)
  assert.equal(finished.kind, 'image')

  const result = await service.admitContent(ADDRESS, [
    { type: 'text', text: 'see' },
    { type: 'image', receiptId: 'r-img' },
  ])
  assert.equal(result.content[0].type, 'text')
  assert.equal(result.content[1].type, 'image')
  assert.equal(result.content[1].attachment.attachmentId, 'image-ref-1')
  assert.equal(store.admitted.length, 1)
  assert.equal(store.admitted[0][1].data, 'AAED')
  service.commit('req-1', result.receipts)
  assert.equal(service.receipts.get('r-img').state, 'committed')
})

test('generic files are saved verbatim through the official stream API', async () => {
  const { service, store } = serviceFor()
  service.begin({
    address: ADDRESS,
    receiptId: 'r-file',
    kind: 'file',
    name: 'notes.txt',
    bytes: 5,
  })
  service.chunk({ receiptId: 'r-file', sequence: 0, bytes: Buffer.from('hello').toString('base64') })
  const finished = await service.finish({ receiptId: 'r-file' })
  assert.equal(finished.ready, true)
  assert.equal(finished.file.attachmentId, 'file-ref-1')
  assert.equal(finished.file.name, 'notes.txt')
  assert.equal(store.saved[0].bytes.toString(), 'hello')

  const result = await service.admitContent(ADDRESS, [
    { type: 'file', receiptId: 'r-file' },
    { type: 'text', text: 'read it' },
  ])
  assert.equal(result.content[0].type, 'file')
  assert.equal(result.content[0].attachment.attachmentId, 'file-ref-1')
  service.commit('req-2', result.receipts)
})

test('receipts are session-bound and cannot be reused after commit', async () => {
  const { service } = serviceFor()
  beginImage(service)
  service.chunk({ receiptId: 'r-img', sequence: 0, bytes: 'AA==' })
  service.chunk({ receiptId: 'r-img', sequence: 1, bytes: 'AQ==' })
  await service.finish({ receiptId: 'r-img' })
  const result = await service.admitContent(ADDRESS, [{ type: 'image', receiptId: 'r-img' }])
  service.commit('req-3', result.receipts)

  await assert.rejects(
    service.admitContent(ADDRESS, [{ type: 'image', receiptId: 'r-img' }]),
    (error) => error.code === 1029,
  )
  assert.throws(
    () => service.begin({ address: ADDRESS, receiptId: 'r-img', kind: 'image', mediaType: 'image/png' }),
    (error) => error.code === 1029,
  )
  assert.throws(
    () => beginImage(service, 'r-other', { address: { rootSessionId: 'other' } }),
    (error) => error.code === 1011,
  )
})

test('subagent addresses accept images but reject generic files', () => {
  const { service } = serviceFor()
  const child = { rootSessionId: ROOT, childSessionId: 'child' }
  assert.throws(
    () => service.begin({ address: child, receiptId: 'child-file', kind: 'file', name: 'a.txt' }),
    (error) => error.code === 1008,
  )
  const image = service.begin({
    address: child,
    receiptId: 'child-image',
    kind: 'image',
    mediaType: 'image/png',
  })
  assert.equal(image.kind, 'image')
})

test('receipt ids and names cannot smuggle local paths', () => {
  const { service } = serviceFor()
  assert.throws(
    () => service.begin({ address: ADDRESS, receiptId: '../escape', kind: 'image', mediaType: 'image/png' }),
    (error) => error.code === -32602,
  )
  service.begin({
    address: ADDRESS,
    receiptId: 'safe-id',
    kind: 'image',
    mediaType: 'image/png',
    name: '/tmp/secret/pixel.png',
  })
  assert.equal(service.receipts.get('safe-id').name, 'pixel.png')
})

test('an incomplete receipt cannot be submitted', async () => {
  const { service } = serviceFor()
  beginImage(service)
  service.chunk({ receiptId: 'r-img', sequence: 0, bytes: 'AA==' })
  await assert.rejects(
    service.admitContent(ADDRESS, [{ type: 'image', receiptId: 'r-img' }]),
    (error) => error.code === 1030,
  )
  assert.throws(
    () => service.chunk({ receiptId: 'missing', sequence: 0, bytes: 'AA==' }),
    (error) => error.code === 1028,
  )
})

test('chunk order, canonical base64, and chunk size are enforced', async () => {
  const { service } = serviceFor()
  beginImage(service)
  assert.throws(
    () => service.chunk({ receiptId: 'r-img', sequence: 1, bytes: 'AA==' }),
    (error) => error.code === 1031,
  )
  assert.throws(
    () => service.chunk({ receiptId: 'r-img', sequence: 0, bytes: 'AA' }),
    (error) => error.code === -32602,
  )
  const oversized = Buffer.alloc(49_153).toString('base64')
  assert.throws(
    () => service.chunk({ receiptId: 'r-img', sequence: 0, bytes: oversized }),
    (error) => error.code === 1003,
  )
})

test('declared byte length is checked at finish', async () => {
  const { service } = serviceFor()
  beginImage(service, 'r-img', { bytes: 10 })
  service.chunk({ receiptId: 'r-img', sequence: 0, bytes: 'AA==' })
  await assert.rejects(service.finish({ receiptId: 'r-img' }), (error) => error.code === 1030)
})

test('official image admission failures keep their stable reason', async () => {
  const { service } = serviceFor({
    store: fakeStore({ failAdmission: attachmentError('IMAGE_TYPE_MISMATCH', 'declared type does not match bytes') }),
  })
  beginImage(service)
  service.chunk({ receiptId: 'r-img', sequence: 0, bytes: 'AA==' })
  service.chunk({ receiptId: 'r-img', sequence: 1, bytes: 'AQ==' })
  await service.finish({ receiptId: 'r-img' })
  await assert.rejects(
    service.admitContent(ADDRESS, [{ type: 'image', receiptId: 'r-img' }]),
    (error) => error.code === 1032 && error.data.reason === 'IMAGE_TYPE_MISMATCH',
  )
  assert.equal(service.receipts.get('r-img').state, 'ready')
})

test('an explicit no-image model refusal is reported before send', async () => {
  const llm = {
    async resolveModelInfo() {
      return { inputModalities: ['text'] }
    },
  }
  const { service } = serviceFor({ llm, config: { provider: 'test', model: 'text-only' } })
  beginImage(service)
  service.chunk({ receiptId: 'r-img', sequence: 0, bytes: 'AA==' })
  service.chunk({ receiptId: 'r-img', sequence: 1, bytes: 'AQ==' })
  await service.finish({ receiptId: 'r-img' })
  await assert.rejects(
    service.admitContent(ADDRESS, [{ type: 'image', receiptId: 'r-img' }]),
    (error) => error.code === 1033 && error.data.reason === 'MODEL_DOES_NOT_SUPPORT_IMAGES',
  )
})

test('per-message image count and total bytes are bounded', async () => {
  const store = fakeStore({
    imageLimits: {
      maxImageBytes: 20 * 1024 * 1024,
      maxImagesPerMessage: 1,
      maxMessageImageBytes: 200 * 1024 * 1024,
      maxImagePixels: 64_000_000,
      maxImageDimension: 8192,
      mediaTypes: ['image/png', 'image/jpeg', 'image/webp', 'image/gif'],
    },
  })
  const { service } = serviceFor({ store })
  for (const receiptId of ['i-1', 'i-2']) {
    service.begin({ address: ADDRESS, receiptId, kind: 'image', mediaType: 'image/png' })
    service.chunk({ receiptId, sequence: 0, bytes: 'AA==' })
    await service.finish({ receiptId })
  }
  await assert.rejects(
    service.admitContent(ADDRESS, [
      { type: 'image', receiptId: 'i-1' },
      { type: 'image', receiptId: 'i-2' },
    ]),
    (error) => error.code === 1034,
  )
  assert.equal(service.receipts.get('i-1').state, 'ready')
  assert.equal(service.receipts.get('i-2').state, 'ready')
})

test('a generic-file save failure is definite and leaves no reusable receipt', async () => {
  const { service } = serviceFor({ store: fakeStore({ files: false }) })
  service.begin({ address: ADDRESS, receiptId: 'r-file', kind: 'file', name: 'a.txt', bytes: 1 })
  service.chunk({ receiptId: 'r-file', sequence: 0, bytes: 'eA==' })
  await assert.rejects(service.finish({ receiptId: 'r-file' }), (error) => error.code === 1035)
  assert.equal(service.receipts.get('r-file').state, 'failed')
})

test('a zero-byte generic file is a valid attachment', async () => {
  const { service } = serviceFor()
  service.begin({ address: ADDRESS, receiptId: 'empty', kind: 'file', name: 'empty.log', bytes: 0 })
  const finished = await service.finish({ receiptId: 'empty' })
  assert.equal(finished.ready, true)
  assert.equal(finished.file.bytes, 0)
  const result = await service.admitContent(ADDRESS, [{ type: 'file', receiptId: 'empty' }])
  assert.equal(result.content[0].attachment.bytes, 0)
})

test('per-message total image bytes are bounded', async () => {
  const store = fakeStore({
    imageLimits: {
      maxImageBytes: 20 * 1024 * 1024,
      maxImagesPerMessage: 20,
      maxMessageImageBytes: 1,
      maxImagePixels: 64_000_000,
      maxImageDimension: 8192,
      mediaTypes: ['image/png'],
    },
  })
  const { service } = serviceFor({ store })
  for (const receiptId of ['i-1', 'i-2']) {
    service.begin({ address: ADDRESS, receiptId, kind: 'image', mediaType: 'image/png' })
    service.chunk({ receiptId, sequence: 0, bytes: 'AA==' })
    await service.finish({ receiptId })
  }
  await assert.rejects(
    service.admitContent(ADDRESS, [
      { type: 'image', receiptId: 'i-1' },
      { type: 'image', receiptId: 'i-2' },
    ]),
    (error) => error.code === 1034,
  )
})

test('bindPrompt lets an inbox observation retire before commit', async () => {
  const { service } = serviceFor()
  service.begin({ address: ADDRESS, receiptId: 'r-file', kind: 'file', name: 'a.txt', bytes: 1 })
  service.chunk({ receiptId: 'r-file', sequence: 0, bytes: 'eA==' })
  await service.finish({ receiptId: 'r-file' })
  const result = await service.admitContent(ADDRESS, [{ type: 'file', receiptId: 'r-file' }])
  service.bindPrompt('req-bind', result.receipts)
  service.observeInbox({
    nextTurn: [{ id: 'm-1', content: [], source: { kind: 'user', rpcId: 'req-bind' } }],
    nextStep: [],
  })
  assert.equal(service.receipts.has('r-file'), false)
  // A later commit must not recreate the retired pending entry.
  service.commit('req-bind', result.receipts)
  assert.equal(service.pending.has('req-bind'), false)
})

test('durable user messages and discarded items retire receipts', async () => {
  const { service } = serviceFor()
  for (const receiptId of ['r-a', 'r-b']) {
    service.begin({ address: ADDRESS, receiptId, kind: 'file', name: `${receiptId}.txt`, bytes: 1 })
    service.chunk({ receiptId, sequence: 0, bytes: 'eA==' })
    await service.finish({ receiptId })
  }
  const first = await service.admitContent(ADDRESS, [{ type: 'file', receiptId: 'r-a' }])
  service.commit('req-a', first.receipts)
  service.observeUserMessage({ source: { kind: 'user', rpcId: 'req-a' } })
  assert.equal(service.receipts.has('r-a'), false)

  const second = await service.admitContent(ADDRESS, [{ type: 'file', receiptId: 'r-b' }])
  service.commit('req-b', second.receipts)
  service.observeDiscarded({ source: { kind: 'user', rpcId: 'req-b' } })
  assert.equal(service.receipts.has('r-b'), false)
})

test('concurrent duplicate submits share one admission and one message', async () => {
  const store = fakeStore({ admissionDelayMs: 20 })
  const { service: attachments, ctx } = serviceFor({ store })
  const agent = fakeAgent()
  agent.status = 'running'
  agent.calls = { followup: [] }
  agent.inbox = { nextTurn: [], nextStep: [] }
  agent.followup = (message) => agent.calls.followup.push(message)
  const conversation = new ConversationService({
    ctx,
    hub: { publish: () => {} },
    agent,
    session: fakeSession(),
    rootSessionId: ROOT,
    attachments,
  })
  attachments.begin({
    address: ADDRESS,
    receiptId: 'r-img',
    kind: 'image',
    mediaType: 'image/png',
  })
  attachments.chunk({ receiptId: 'r-img', sequence: 0, bytes: 'AA==' })
  await attachments.finish({ receiptId: 'r-img' })
  const params = {
    address: ADDRESS,
    requestId: 'same-request',
    delivery: 'queue',
    content: [{ type: 'image', receiptId: 'r-img' }],
  }
  const [first, second] = await Promise.all([
    conversation.submit(params),
    conversation.submit(params),
  ])
  assert.equal(first.messageId, second.messageId)
  assert.equal(agent.calls.followup.length, 1)
  assert.equal(store.admitted.length, 1)
})

test('inbox observation retires committed receipts', async () => {
  const { service } = serviceFor()
  service.begin({ address: ADDRESS, receiptId: 'r-file', kind: 'file', name: 'a.txt', bytes: 1 })
  service.chunk({ receiptId: 'r-file', sequence: 0, bytes: 'eA==' })
  await service.finish({ receiptId: 'r-file' })
  const result = await service.admitContent(ADDRESS, [{ type: 'file', receiptId: 'r-file' }])
  service.commit('req-4', result.receipts)
  service.observeInbox({
    nextTurn: [{ id: 'm-1', content: [], source: { kind: 'user', rpcId: 'req-4' } }],
    nextStep: [],
  })
  assert.equal(service.receipts.has('r-file'), false)
  assert.throws(
    () => service.chunk({ receiptId: 'r-file', sequence: 0, bytes: 'eA==' }),
    (error) => error.code === 1028,
  )
})

test('discard removes staged bytes but refuses a committed receipt', async () => {
  const { service, dataRoot } = serviceFor()
  service.begin({ address: ADDRESS, receiptId: 'r-file', kind: 'file', name: 'a.txt' })
  const staging = path.join(dataRoot, 'runtime', 'attachment-staging')
  const temp = fs
    .readdirSync(staging)
    .map((name) => path.join(staging, name))
    .find((name) => name.endsWith('.part'))
  assert.ok(temp, 'a staging file exists')
  assert.deepEqual(service.discard({ receiptId: 'r-file' }), { discarded: true })
  assert.equal(fs.existsSync(temp), false)

  beginImage(service, 'r-img')
  service.chunk({ receiptId: 'r-img', sequence: 0, bytes: 'AA==' })
  service.chunk({ receiptId: 'r-img', sequence: 1, bytes: 'AQ==' })
  await service.finish({ receiptId: 'r-img' })
  const result = await service.admitContent(ADDRESS, [{ type: 'image', receiptId: 'r-img' }])
  service.commit('req-5', result.receipts)
  assert.deepEqual(service.discard({ receiptId: 'r-img' }), { discarded: false })
})

test('file-reference discovery lists paths and reports an absent provider', async () => {
  const refs = {
    async list(_agent, query) {
      return [
        { path: `src/${query}.rs`, kind: 'file' },
        { path: `src/${query}`, kind: 'directory' },
      ]
    },
  }
  const { service } = serviceFor({ refs })
  const result = await service.listReferences({ address: ADDRESS, query: 'main' })
  assert.deepEqual(result.candidates, [
    { path: 'src/main.rs', kind: 'file' },
    { path: 'src/main', kind: 'directory' },
  ])

  const missing = serviceFor({ refs: undefined }).service
  await assert.rejects(
    missing.listReferences({ address: ADDRESS, query: '' }),
    (error) => error.code === 1008,
  )
})

test('a cancelled file-reference request is reported as cancelled', async () => {
  const refs = {
    async list(_agent, _query, signal) {
      await new Promise((resolve) => setTimeout(resolve, 20))
      if (signal.aborted) throw new Error('aborted')
      return []
    },
  }
  const { service } = serviceFor({ refs })
  const controller = new AbortController()
  const pending = service.listReferences({ address: ADDRESS, query: 'x' }, controller.signal)
  controller.abort()
  await assert.rejects(pending, (error) => error.code === 1006)
})

test('conversation submit resolves file receipts before the Agent sees the message', async () => {
  const { service: attachments, ctx } = serviceFor()
  const agent = fakeAgent()
  agent.calls = { followup: [] }
  agent.followup = (message) => agent.calls.followup.push(message)
  agent.inbox = { nextTurn: [], nextStep: [] }
  const hub = { publish: () => {} }
  const conversation = new ConversationService({
    ctx,
    hub,
    agent,
    session: fakeSession(),
    rootSessionId: ROOT,
    attachments,
  })
  attachments.begin({ address: ADDRESS, receiptId: 'r-file', kind: 'file', name: 'a.txt', bytes: 1 })
  attachments.chunk({ receiptId: 'r-file', sequence: 0, bytes: 'eA==' })
  await attachments.finish({ receiptId: 'r-file' })
  const result = await conversation.submit({
    address: ADDRESS,
    requestId: 'req-6',
    delivery: 'queue',
    content: [{ type: 'file', receiptId: 'r-file' }, { type: 'text', text: 'read' }],
  })
  assert.equal(result.accepted, true)
  assert.equal(agent.calls.followup.length, 1)
  assert.equal(agent.calls.followup[0].content[0].type, 'file')
  assert.equal(agent.calls.followup[0].content[1].text, 'read')
})
