// Transport selection: dedicated fd 3/4 first, stdin/stdout fallback with a
// per-generation nonce. The bridge never writes protocol frames to stdout when
// the dedicated descriptors are present, so third-party stdout noise cannot be
// mistaken for events.
import fs from 'node:fs'
import { StartupError } from './errors.mjs'

/**
 * Open the protocol transport for one host generation.
 * @param {{nonce?: string}} options - fallback nonce supplied by the Rust runtime.
 * @returns {{
 *   kind: 'fd'|'stdio',
 *   nonceRequired: boolean,
 *   input: AsyncIterable<Buffer>,
 *   write: (buffer: Buffer) => Promise<void>,
 *   close: () => void
 * }}
 */
export function openTransport({ nonce, kind } = {}) {
  if (kind === 'stdio') {
    if (typeof nonce !== 'string' || nonce === '') {
      throw new StartupError('unsupported', 'the stdio fallback requires a per-generation nonce')
    }
    return createStdioTransport()
  }
  if (kind === undefined || kind === 'fd') {
    if (descriptorUsable(3) && descriptorUsable(4)) {
      return createFdTransport()
    }
    if (kind === 'fd') {
      throw new StartupError('unsupported', 'the runtime requested fd 3/4, but those descriptors are not usable')
    }
  }
  if (typeof nonce !== 'string' || nonce === '') {
    throw new StartupError('unsupported', 'dedicated fd 3/4 are unavailable and no fallback nonce was supplied')
  }
  return createStdioTransport()
}

/**
 * @param {number} fd - descriptor number.
 * @returns {boolean} whether the descriptor is an open pipe or socket.
 */
function descriptorUsable(fd) {
  try {
    const stat = fs.fstatSync(fd)
    return stat.isFIFO() || stat.isSocket()
  } catch {
    return false
  }
}

function createFdTransport() {
  const input = fs.createReadStream(null, { fd: 3, autoClose: false })
  input.on('error', () => {
    // Surface read failures through async iteration; an explicit handler avoids
    // an unhandled 'error' event when the Rust process dies.
  })
  return {
    kind: 'fd',
    nonceRequired: false,
    input,
    write: (buffer) => writeAll(4, buffer),
    close: () => {
      // autoClose:false keeps the descriptor open for the lifetime of the
      // process; close it explicitly so the event loop can drain.
      input.destroy()
      try {
        fs.closeSync(3)
      } catch {
        // already closed
      }
      try {
        fs.closeSync(4)
      } catch {
        // already closed
      }
    },
  }
}

function createStdioTransport() {
  process.stdin.resume()
  return {
    kind: 'stdio',
    nonceRequired: true,
    input: process.stdin,
    write: (buffer) => writeStream(process.stdout, buffer),
    close: () => {
      process.stdin.pause()
    },
  }
}

/**
 * Write all bytes to a descriptor, tolerating partial writes.
 * @param {number} fd - destination descriptor.
 * @param {Buffer} buffer - complete frame including newline.
 * @returns {Promise<void>} resolves after every byte was accepted.
 */
function writeAll(fd, buffer) {
  return new Promise((resolve, reject) => {
    let offset = 0
    const step = () => {
      fs.write(fd, buffer, offset, buffer.length - offset, null, (error, written) => {
        if (error) {
          reject(error)
          return
        }
        offset += written
        if (offset >= buffer.length) {
          resolve()
        } else {
          step()
        }
      })
    }
    step()
  })
}

/**
 * Write all bytes to a Node stream, honoring backpressure.
 * @param {NodeJS.WritableStream} stream - destination stream.
 * @param {Buffer} buffer - complete frame including newline.
 * @returns {Promise<void>} resolves after the stream accepted the bytes.
 */
function writeStream(stream, buffer) {
  return new Promise((resolve, reject) => {
    const onError = (error) => {
      stream.off('drain', onDrain)
      reject(error)
    }
    const onDrain = () => {
      stream.off('error', onError)
      resolve()
    }
    stream.once('error', onError)
    if (stream.write(buffer)) {
      stream.off('error', onError)
      resolve()
    } else {
      stream.once('drain', onDrain)
    }
  })
}
