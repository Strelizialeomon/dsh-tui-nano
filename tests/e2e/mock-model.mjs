// Local Messages-compatible SSE endpoint for the S13 e2e tests.
// The model is not called; the response proves the real persistence path while
// keeping the acceptance free of model spend.
import http from 'node:http'

const EVENTS = [
  { type: 'message_start', message: { usage: { input_tokens: 1, output_tokens: 0 } } },
  { type: 'content_block_start', index: 0, content_block: { type: 'text', text: '' } },
  { type: 'content_block_delta', index: 0, delta: { type: 'text_delta', text: 'S13-MOCK-OK' } },
  { type: 'content_block_stop', index: 0 },
  { type: 'message_delta', delta: { stop_reason: 'end_turn' }, usage: { output_tokens: 1 } },
  { type: 'message_stop' },
]

export function startMockModel() {
  let requests = 0
  const server = http.createServer((request, response) => {
    if (request.method === 'POST' && request.url.includes('messages')) requests += 1
    request.resume()
    request.on('end', () => {
      const body = EVENTS.map(
        (event) => `event: ${event.type}\ndata: ${JSON.stringify(event)}\n\n`,
      ).join('')
      response.writeHead(200, {
        'content-type': 'text/event-stream',
        'content-length': Buffer.byteLength(body),
      })
      response.end(body)
    })
  })
  return new Promise((resolve) => {
    server.listen(0, '127.0.0.1', () => {
      const { port } = server.address()
      resolve({
        url: `http://127.0.0.1:${port}`,
        requests: () => requests,
        close: () => new Promise((done) => server.close(done)),
      })
    })
  })
}
