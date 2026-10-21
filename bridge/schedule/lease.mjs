// Per-root-session schedule lease: one host at a time may drive the schedule
// table. The lease is a second gate behind S01's mount ledger; it exists so a
// ledger bug cannot produce two timers for the same table.
//
// The file is `<dataRoot>/runtime-data/<rootSessionId>/schedule.lock`, created
// with O_EXCL and mode 0600 inside a 0700 directory. There is no heartbeat:
// liveness is `process.kill(pid, 0)`, and a crashed owner is swept by the next
// claimant. A lock whose content cannot be parsed is only reclaimed after its
// mtime is at least 30 seconds old; before that the state is unknown and the
// host refuses instead of guessing.
import fs from 'node:fs'
import path from 'node:path'
import { randomUUID } from 'node:crypto'

/** File name of the lease inside the root session's runtime-data directory. */
export const SCHEDULE_LOCK_NAME = 'schedule.lock'

/** Reclaim threshold for an unreadable lease, in milliseconds. */
export const SCHEDULE_LOCK_STALE_MS = 30_000

const LOCK_VERSION = 1

/** A schedule lease failure with a stable classification. */
export class ScheduleLeaseError extends Error {
  /**
   * @param {'held'|'unknown'} code - `held` means a live host owns the lease;
   *   `unknown` means the file exists but its state cannot be established.
   * @param {string} message - user-facing description.
   * @param {{holderPid?: number, cause?: unknown}} [extra] - structured details.
   */
  constructor(code, message, extra = {}) {
    super(message, extra.cause === undefined ? undefined : { cause: extra.cause })
    this.name = 'ScheduleLeaseError'
    this.code = code
    this.holderPid = extra.holderPid
  }
}

/**
 * Acquire the lease for one root session.
 * @param {{dataRoot: string, rootSessionId: string, pid?: number, now?: () => Date}} options - lease inputs.
 * @returns {{file: string, sessionId: string, hostPid: number, adopted: boolean}} the held lease.
 * @throws {ScheduleLeaseError} when another live host holds it or the state is unknown.
 */
export function acquireScheduleLease({ dataRoot, rootSessionId, pid = process.pid, now = () => new Date() }) {
  const directory = path.join(dataRoot, 'runtime-data', rootSessionId)
  fs.mkdirSync(directory, { recursive: true, mode: 0o700 })
  fs.chmodSync(directory, 0o700)
  const file = path.join(directory, SCHEDULE_LOCK_NAME)

  for (let attempt = 0; attempt < 2; attempt += 1) {
    try {
      const content = { version: LOCK_VERSION, sessionId: rootSessionId, hostPid: pid, acquiredAt: now().toISOString() }
      createLease(file, content)
      return { file, sessionId: rootSessionId, hostPid: pid, adopted: false }
    } catch (error) {
      if (error?.code !== 'EEXIST') {
        throw new ScheduleLeaseError('unknown', `无法创建调度锁 ${file}：${describe(error)}`, { cause: error })
      }
    }

    const inspected = inspectLease(file, rootSessionId)
    if (inspected.kind === 'absent') continue
    if (inspected.kind === 'corrupt') {
      const age = ageOf(file)
      if (age === undefined) continue
      if (age < SCHEDULE_LOCK_STALE_MS) {
        throw new ScheduleLeaseError(
          'unknown',
          `调度锁状态未知（${file} 不可解析且写入不足 ${Math.ceil(SCHEDULE_LOCK_STALE_MS / 1000)} 秒）`,
        )
      }
      reclaimStale(file, rootSessionId, pid, now)
      continue
    }
    if (inspected.content.hostPid === pid) {
      return { file, sessionId: rootSessionId, hostPid: pid, adopted: true }
    }
    if (isPidAlive(inspected.content.hostPid)) {
      throw new ScheduleLeaseError('held', `调度由 pid ${inspected.content.hostPid} 持有`, {
        holderPid: inspected.content.hostPid,
      })
    }
    reclaimStale(file, rootSessionId, pid, now)
  }

  throw new ScheduleLeaseError('unknown', `调度锁在两次尝试后仍无法获取：${file}`)
}

/**
 * Remove a stale lock under a one-writer reclaim guard.
 *
 * The guard is what stops two hosts that both read the same dead owner from
 * deleting each other's freshly created lock: only the guard holder unlinks,
 * and it re-reads the target after taking the guard. Without this, B could
 * unlink the live lock C created between B's read and B's unlink, and both
 * would drive one table.
 * @param {string} file - lease file path.
 * @param {string} sessionId - root session.
 * @param {number} pid - this process.
 * @param {() => Date} now - clock for the guard content.
 */
function reclaimStale(file, sessionId, pid, now) {
  const guard = `${file}.reclaim`
  acquireReclaimGuard(guard, sessionId, pid, now)
  try {
    const inspected = inspectLease(file, sessionId)
    if (inspected.kind === 'absent') return
    if (inspected.kind === 'valid') {
      if (inspected.content.hostPid === pid) return
      if (isPidAlive(inspected.content.hostPid)) {
        throw new ScheduleLeaseError('held', `调度由 pid ${inspected.content.hostPid} 持有`, {
          holderPid: inspected.content.hostPid,
        })
      }
      unlink(file)
      return
    }
    const age = ageOf(file)
    if (age !== undefined && age < SCHEDULE_LOCK_STALE_MS) {
      throw new ScheduleLeaseError('unknown', `调度锁状态未知（${file} 仍是新写入且不可解析）`)
    }
    unlink(file)
  } finally {
    unlink(guard)
  }
}

function acquireReclaimGuard(guard, sessionId, pid, now) {
  for (let attempt = 0; attempt < 2; attempt += 1) {
    try {
      createLease(guard, { version: LOCK_VERSION, sessionId, hostPid: pid, acquiredAt: now().toISOString() })
      return
    } catch (error) {
      if (error?.code !== 'EEXIST') {
        throw new ScheduleLeaseError('unknown', `无法创建调度锁回收保护 ${guard}：${describe(error)}`, { cause: error })
      }
    }
    const inspected = inspectLease(guard, sessionId)
    if (inspected.kind === 'absent') continue
    const age = ageOf(guard)
    if (inspected.kind === 'valid' && isPidAlive(inspected.content.hostPid)) {
      throw new ScheduleLeaseError('unknown', '另一个宿主正在回收调度锁；请稍后重试')
    }
    if (inspected.kind === 'corrupt' && age !== undefined && age < SCHEDULE_LOCK_STALE_MS) {
      throw new ScheduleLeaseError('unknown', '调度锁回收保护状态未知')
    }
    unlink(guard)
  }
  throw new ScheduleLeaseError('unknown', `无法取得调度锁回收保护：${guard}`)
}


/**
 * Release a lease only when it still belongs to this process and session.
 * @param {{file: string, sessionId: string, hostPid: number}} lease - held lease.
 * @returns {boolean} whether this call removed the file.
 */
export function releaseScheduleLease(lease) {
  if (lease === undefined || lease === null) return false
  const inspected = inspectLease(lease.file, lease.sessionId)
  if (inspected.kind !== 'valid') return false
  if (inspected.content.hostPid !== lease.hostPid || inspected.content.sessionId !== lease.sessionId) return false
  unlink(lease.file)
  return true
}

/**
 * Whether `process.kill(pid, 0)` reports the process as present. `EPERM` still
 * means the process exists; only `ESRCH` means it is gone.
 * @param {number} pid - candidate owner pid.
 * @returns {boolean} liveness.
 */
export function isPidAlive(pid) {
  if (!Number.isInteger(pid) || pid <= 0) return false
  try {
    process.kill(pid, 0)
    return true
  } catch (error) {
    return error?.code !== 'ESRCH'
  }
}

function createLease(file, content) {
  // Write the complete document to a private temp file, then publish it with
  // link(), which fails with EEXIST if a contender got there first. A reader
  // therefore never sees a freshly created but half-written lock.
  const temp = path.join(path.dirname(file), `.${path.basename(file)}.${process.pid}.${randomUUID()}.tmp`)
  try {
    const fd = fs.openSync(temp, 'wx', 0o600)
    try {
      fs.writeFileSync(fd, `${JSON.stringify(content)}\n`, 'utf8')
      fs.fsyncSync(fd)
    } finally {
      fs.closeSync(fd)
    }
    fs.linkSync(temp, file)
  } finally {
    try {
      fs.unlinkSync(temp)
    } catch {
      // The temp file is private to this call and may already be gone.
    }
  }
  fs.chmodSync(file, 0o600)
}

function inspectLease(file, sessionId) {
  let text
  try {
    text = fs.readFileSync(file, 'utf8')
  } catch (error) {
    if (error?.code === 'ENOENT') return { kind: 'absent' }
    return { kind: 'corrupt' }
  }
  let content
  try {
    content = JSON.parse(text)
  } catch {
    return { kind: 'corrupt' }
  }
  if (
    content === null ||
    typeof content !== 'object' ||
    Array.isArray(content) ||
    content.version !== LOCK_VERSION ||
    content.sessionId !== sessionId ||
    !Number.isInteger(content.hostPid) ||
    content.hostPid <= 0 ||
    typeof content.acquiredAt !== 'string'
  ) {
    return { kind: 'corrupt' }
  }
  return { kind: 'valid', content }
}

function ageOf(file) {
  try {
    return Date.now() - fs.statSync(file).mtimeMs
  } catch {
    return undefined
  }
}

function unlink(file) {
  try {
    fs.unlinkSync(file)
  } catch (error) {
    if (error?.code !== 'ENOENT') throw error
  }
}

function describe(error) {
  return error instanceof Error ? error.message : String(error)
}
