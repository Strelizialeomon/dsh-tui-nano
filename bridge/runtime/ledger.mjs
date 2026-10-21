// Session mount ledger: the same-machine exclusion that covers blank sessions
// before the official per-session write lock exists. One record per host
// process; pid liveness is the only liveness rule, and a corrupt ledger is a
// hard startup failure rather than an empty ledger.
import fs from 'node:fs'
import path from 'node:path'
import { randomBytes } from 'node:crypto'
import { LIMITS } from '../../protocol/index.mjs'
import { StartupError } from './errors.mjs'

export const LEDGER_VERSION = 1
export const LEDGER_FILE = 'session-mounts.json'
export const LOCK_FILE = 'session-mounts.lock'

/**
 * Claim one or more root sessions for this host process.
 * @param {{
 *   dataRoot: string,
 *   pid: number,
 *   sessionIds: string[],
 *   token?: string,
 *   staleMs?: number,
 *   now?: () => number,
 *   isAlive?: (pid: number) => boolean,
 *   sleep?: (ms: number) => Promise<void>,
 * }} options - claim inputs; pid is the host process, not the window process.
 * @returns {Promise<{token: string, pid: number, sessionIds: string[], dataRoot: string, ledgerPath: string, lockPath: string}>}
 *   release handle for {@link releaseMount}.
 * @throws {StartupError} `occupied` when another live owner holds a session,
 *   `startup-failed` when the ledger or lock is unreadable or malformed.
 */
export async function claimMount({
  dataRoot,
  pid,
  sessionIds,
  token = `${String(pid)}-${randomBytes(16).toString('hex')}`,
  staleMs = LIMITS.mountLockStaleMs,
  now = () => Date.now(),
  isAlive = defaultIsAlive,
  sleep = defaultSleep,
}) {
  if (!Array.isArray(sessionIds) || sessionIds.length === 0 || sessionIds.some((id) => typeof id !== 'string' || id === '')) {
    throw new TypeError('sessionIds must be a non-empty array of non-empty strings')
  }
  ensureDataRoot(dataRoot)
  const ledgerPath = path.join(dataRoot, LEDGER_FILE)
  const lockPath = path.join(dataRoot, LOCK_FILE)

  await acquireLock({ lockPath, token, staleMs, now, isAlive, sleep })
  try {
    const ledger = readLedger(ledgerPath)
    const liveOwners = ledger.owners.filter((owner) => isAlive(owner.pid))
    for (const sessionId of sessionIds) {
      const holder = liveOwners.find((owner) => owner.token !== token && owner.sessionIds.includes(sessionId))
      if (holder !== undefined) {
        throw new StartupError('occupied', `root session ${sessionId} is owned by pid ${String(holder.pid)}`, {
          holderPid: holder.pid,
          repairPath: ledgerPath,
        })
      }
    }
    const owners = liveOwners.filter((owner) => owner.token !== token)
    owners.push({
      pid,
      startedAt: new Date(now()).toISOString(),
      sessionIds: [...sessionIds],
      token,
    })
    assertLockHeld(lockPath, token)
    atomicWriteJson(ledgerPath, { version: LEDGER_VERSION, owners })
    return { token, pid, sessionIds: [...sessionIds], dataRoot, ledgerPath, lockPath }
  } finally {
    releaseLock(lockPath, token)
  }
}

/**
 * Release this host's ledger record. The lock is taken again so a concurrent
 * claimant cannot lose the update; a missing record counts as already released.
 * @param {{token: string, ledgerPath: string, lockPath: string, staleMs?: number, now?: () => number,
 *   isAlive?: (pid: number) => boolean, sleep?: (ms: number) => Promise<void>}} claim - handle from claimMount.
 * @returns {Promise<void>} resolves after the record and lock are gone.
 * @throws {StartupError} `cleanup-failed` when the ledger cannot be read or written.
 */
export async function releaseMount(claim) {
  const {
    token,
    ledgerPath,
    lockPath,
    staleMs = LIMITS.mountLockStaleMs,
    now = () => Date.now(),
    isAlive = defaultIsAlive,
    sleep = defaultSleep,
  } = claim
  try {
    await acquireLock({ lockPath, token, staleMs, now, isAlive, sleep })
  } catch (error) {
    throw asCleanupFailure('could not acquire the mount lock for release', error)
  }
  try {
    let ledger
    try {
      ledger = readLedger(ledgerPath)
    } catch (error) {
      throw asCleanupFailure('could not read the mount ledger for release', error)
    }
    const owners = ledger.owners.filter((owner) => owner.token !== token)
    if (owners.length !== ledger.owners.length) {
      atomicWriteJson(ledgerPath, { version: LEDGER_VERSION, owners })
    }
  } catch (error) {
    throw asCleanupFailure('could not write the mount ledger during release', error)
  } finally {
    releaseLock(lockPath, token)
  }
}

/**
 * Read and validate the ledger. A missing file is an empty ledger; any other
 * failure is a hard error with a repair path.
 * @param {string} ledgerPath - absolute ledger path.
 * @returns {{version: number, owners: Array<{pid: number, startedAt: string, sessionIds: string[], token: string}>}}
 */
export function readLedger(ledgerPath) {
  let text
  try {
    text = fs.readFileSync(ledgerPath, 'utf8')
  } catch (error) {
    if (error.code === 'ENOENT') return { version: LEDGER_VERSION, owners: [] }
    throw new StartupError('startup-failed', `session mount ledger is unreadable: ${ledgerPath}`, {
      repairPath: ledgerPath,
      cause: error,
    })
  }
  let value
  try {
    value = JSON.parse(text)
  } catch (error) {
    throw new StartupError('startup-failed', `session mount ledger is not valid JSON: ${ledgerPath}`, {
      repairPath: ledgerPath,
      cause: error,
    })
  }
  validateLedger(value, ledgerPath)
  return value
}

function validateLedger(value, ledgerPath) {
  if (value === null || typeof value !== 'object' || Array.isArray(value) || value.version !== LEDGER_VERSION) {
    throw new StartupError('startup-failed', `session mount ledger has an unknown shape or version: ${ledgerPath}`, {
      repairPath: ledgerPath,
    })
  }
  if (!Array.isArray(value.owners)) {
    throw new StartupError('startup-failed', `session mount ledger owners is not an array: ${ledgerPath}`, {
      repairPath: ledgerPath,
    })
  }
  for (const owner of value.owners) {
    const valid =
      owner !== null &&
      typeof owner === 'object' &&
      Number.isInteger(owner.pid) &&
      owner.pid > 0 &&
      (typeof owner.startedAt === 'string' || typeof owner.startedAt === 'number') &&
      typeof owner.token === 'string' &&
      owner.token !== '' &&
      Array.isArray(owner.sessionIds) &&
      owner.sessionIds.length > 0 &&
      owner.sessionIds.every((id) => typeof id === 'string' && id !== '')
    if (!valid) {
      throw new StartupError('startup-failed', `session mount ledger has a malformed owner record: ${ledgerPath}`, {
        repairPath: ledgerPath,
      })
    }
  }
}

async function acquireLock({ lockPath, token, staleMs, now, isAlive, sleep }) {
  const startedAt = now()
  while (true) {
    try {
      const fd = fs.openSync(lockPath, 'wx', 0o600)
      try {
        fs.writeSync(fd, token)
        fs.fsyncSync(fd)
      } finally {
        fs.closeSync(fd)
      }
      return
    } catch (error) {
      if (error.code !== 'EEXIST') {
        throw new StartupError('startup-failed', `could not create the mount lock: ${lockPath}`, { cause: error })
      }
    }

    let content
    let stat
    try {
      content = fs.readFileSync(lockPath, 'utf8')
      stat = fs.statSync(lockPath)
    } catch (error) {
      if (error.code === 'ENOENT') continue
      throw new StartupError('startup-failed', `could not read the mount lock: ${lockPath}`, { cause: error })
    }

    const holderPid = parseLockPid(content)
    if (holderPid !== undefined) {
      if (!isAlive(holderPid)) {
        safeUnlink(lockPath)
        continue
      }
      // A live holder's lock is normally short-lived; retry briefly before
      // declaring the session occupied. A live pid is never reclaimed.
      if (now() - startedAt < 1000) {
        await sleep(50)
        continue
      }
      throw new StartupError('occupied', `session mount lock is held by pid ${String(holderPid)}`, {
        holderPid,
      })
    }

    const age = now() - stat.mtimeMs
    if (age > staleMs) {
      safeUnlink(lockPath)
      continue
    }
    if (now() - startedAt > staleMs + 5000) {
      throw new StartupError('occupied', 'session mount lock has no valid owner and did not age out')
    }
    await sleep(Math.max(10, Math.min(250, staleMs - age + 10)))
  }
}

function assertLockHeld(lockPath, token) {
  let content
  try {
    content = fs.readFileSync(lockPath, 'utf8')
  } catch (error) {
    throw new StartupError('startup-failed', 'mount lock disappeared before commit', { cause: error })
  }
  if (content !== token) {
    throw new StartupError('occupied', 'mount lock ownership changed before commit; refusing to overwrite the new holder')
  }
}

function releaseLock(lockPath, token) {
  try {
    if (fs.readFileSync(lockPath, 'utf8') === token) fs.unlinkSync(lockPath)
  } catch {
    // A missing lock is already released; a different token belongs to a new holder.
  }
}

function parseLockPid(content) {
  const match = /^(\d+)-.+$/.exec(content.trim())
  if (match === null) return undefined
  const pid = Number(match[1])
  return Number.isSafeInteger(pid) && pid > 0 ? pid : undefined
}

function ensureDataRoot(dataRoot) {
  try {
    fs.mkdirSync(dataRoot, { recursive: true, mode: 0o700 })
    fs.chmodSync(dataRoot, 0o700)
  } catch (error) {
    throw new StartupError('startup-failed', `could not prepare the Nano data root: ${dataRoot}`, { cause: error })
  }
}

function atomicWriteJson(targetPath, value) {
  const directory = path.dirname(targetPath)
  const temporary = path.join(directory, `.${path.basename(targetPath)}.tmp-${String(process.pid)}-${randomBytes(8).toString('hex')}`)
  let fd
  try {
    fd = fs.openSync(temporary, 'wx', 0o600)
    fs.writeSync(fd, `${JSON.stringify(value, null, 2)}\n`)
    fs.fsyncSync(fd)
    fs.closeSync(fd)
    fd = undefined
    fs.renameSync(temporary, targetPath)
    try {
      const directoryFd = fs.openSync(directory, 'r')
      try {
        fs.fsyncSync(directoryFd)
      } finally {
        fs.closeSync(directoryFd)
      }
    } catch {
      // Directory fsync is best-effort on platforms that refuse it.
    }
  } catch (error) {
    if (fd !== undefined) {
      try {
        fs.closeSync(fd)
      } catch {
        // already closed
      }
    }
    safeUnlink(temporary)
    throw error
  }
}

function safeUnlink(targetPath) {
  try {
    fs.unlinkSync(targetPath)
  } catch {
    // race with another claimant or already gone
  }
}

function defaultIsAlive(pid) {
  try {
    process.kill(pid, 0)
    return true
  } catch (error) {
    if (error.code === 'EPERM') return true
    if (error.code === 'ESRCH') return false
    // Unknown probe failure: treat as alive so we never double-write.
    return true
  }
}

function defaultSleep(ms) {
  return new Promise((resolve) => setTimeout(resolve, ms))
}

function asCleanupFailure(message, cause) {
  if (cause instanceof StartupError && cause.code === 'cleanup-failed') return cause
  return new StartupError('cleanup-failed', `${message}: ${cause instanceof Error ? cause.message : String(cause)}`, {
    cause,
    repairPath: undefined,
  })
}
