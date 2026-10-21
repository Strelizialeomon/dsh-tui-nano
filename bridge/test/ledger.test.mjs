// Session mount ledger tests: atomic claim, live-owner refusal, dead-owner
// sweep, corrupt-ledger refusal, stale lock recovery, and release.
import test from 'node:test'
import assert from 'node:assert/strict'
import fs from 'node:fs'
import os from 'node:os'
import path from 'node:path'

import {
  LEDGER_FILE,
  LOCK_FILE,
  claimMount,
  readLedger,
  releaseMount,
} from '../runtime/ledger.mjs'
import { StartupError } from '../runtime/errors.mjs'

function tempRoot() {
  return fs.mkdtempSync(path.join(os.tmpdir(), 'nano-ledger-'))
}

const live = (pids) => (pid) => pids.includes(pid)

test('claims, records one owner, and releases', async () => {
  const dataRoot = tempRoot()
  const claim = await claimMount({
    dataRoot,
    pid: 1111,
    sessionIds: ['session-a'],
    isAlive: live([1111]),
    now: () => Date.now(),
  })
  const ledger = readLedger(path.join(dataRoot, LEDGER_FILE))
  assert.equal(ledger.owners.length, 1)
  assert.equal(ledger.owners[0].pid, 1111)
  assert.deepEqual(ledger.owners[0].sessionIds, ['session-a'])
  assert.match(ledger.owners[0].token, /^1111-/)
  assert.equal(fs.statSync(dataRoot).mode & 0o777, 0o700)
  assert.equal(fs.statSync(path.join(dataRoot, LEDGER_FILE)).mode & 0o777, 0o600)
  assert.equal(fs.existsSync(path.join(dataRoot, LOCK_FILE)), false)

  await releaseMount(claim)
  assert.deepEqual(readLedger(path.join(dataRoot, LEDGER_FILE)).owners, [])
  assert.equal(fs.existsSync(path.join(dataRoot, LOCK_FILE)), false)
})

test('refuses a live owner of the same root session', async () => {
  const dataRoot = tempRoot()
  const first = await claimMount({
    dataRoot,
    pid: 1111,
    sessionIds: ['session-a'],
    isAlive: live([1111, 2222]),
  })
  await assert.rejects(
    claimMount({
      dataRoot,
      pid: 2222,
      sessionIds: ['session-a'],
      isAlive: live([1111, 2222]),
    }),
    (error) =>
      error instanceof StartupError && error.code === 'occupied' && error.holderPid === 1111,
  )
  // A different session is allowed.
  const other = await claimMount({
    dataRoot,
    pid: 2222,
    sessionIds: ['session-b'],
    isAlive: live([1111, 2222]),
  })
  assert.equal(readLedger(path.join(dataRoot, LEDGER_FILE)).owners.length, 2)
  await releaseMount(first)
  await releaseMount(other)
})

test('sweeps a dead owner and lets a new host claim the session', async () => {
  const dataRoot = tempRoot()
  const first = await claimMount({
    dataRoot,
    pid: 1111,
    sessionIds: ['session-a'],
    isAlive: live([1111]),
  })
  const second = await claimMount({
    dataRoot,
    pid: 2222,
    sessionIds: ['session-a'],
    isAlive: live([2222]),
  })
  const owners = readLedger(path.join(dataRoot, LEDGER_FILE)).owners
  assert.equal(owners.length, 1)
  assert.equal(owners[0].pid, 2222)
  await releaseMount(second)
  // The first claim is already swept; releasing it must not remove the new owner.
  await releaseMount(first)
  assert.equal(readLedger(path.join(dataRoot, LEDGER_FILE)).owners.length, 0)
})

test('refuses a corrupt or unknown-version ledger', async () => {
  for (const content of [
    '{ not json',
    JSON.stringify({ version: 99, owners: [] }),
    JSON.stringify({ version: 1, owners: [{ pid: 'x' }] }),
  ]) {
    const dataRoot = tempRoot()
    fs.writeFileSync(path.join(dataRoot, LEDGER_FILE), content)
    await assert.rejects(
      claimMount({ dataRoot, pid: 1111, sessionIds: ['session-a'], isAlive: live([1111]) }),
      (error) => error instanceof StartupError && error.code === 'startup-failed' && Boolean(error.repairPath),
    )
    assert.equal(fs.existsSync(path.join(dataRoot, LOCK_FILE)), false)
  }
})

test('reclaims an invalid lock only after it is older than the stale window', async () => {
  const dataRoot = tempRoot()
  const lockPath = path.join(dataRoot, LOCK_FILE)
  fs.writeFileSync(lockPath, 'garbage-without-a-pid')
  const old = new Date(Date.now() - 31_000)
  fs.utimesSync(lockPath, old, old)
  const claim = await claimMount({
    dataRoot,
    pid: 1111,
    sessionIds: ['session-a'],
    isAlive: live([1111]),
    staleMs: 30_000,
  })
  assert.equal(readLedger(path.join(dataRoot, LEDGER_FILE)).owners[0].pid, 1111)
  await releaseMount(claim)

  // An invalid lock younger than the window is not reclaimed immediately: the
  // claimant waits until it has stayed invalid for the full window, then takes it.
  const freshRoot = tempRoot()
  fs.writeFileSync(path.join(freshRoot, LOCK_FILE), 'garbage-without-a-pid')
  const start = Date.now()
  let clock = start
  const sleep = async (ms) => {
    clock += ms
    await new Promise((resolve) => setImmediate(resolve))
  }
  const waited = await claimMount({
    dataRoot: freshRoot,
    pid: 2222,
    sessionIds: ['session-b'],
    isAlive: live([2222]),
    staleMs: 200,
    now: () => clock,
    sleep,
  })
  assert.ok(clock - start >= 200, 'the claimant must wait out the stale window')
  assert.equal(readLedger(path.join(freshRoot, LEDGER_FILE)).owners[0].pid, 2222)
  await releaseMount(waited)
})

test('reports a long-held lock as lock-busy, not as the session owner', async () => {
  const dataRoot = tempRoot()
  fs.writeFileSync(path.join(dataRoot, LOCK_FILE), '9999-live-token')
  let clock = Date.now()
  const sleep = async (ms) => {
    clock += ms
    await new Promise((resolve) => setImmediate(resolve))
  }
  await assert.rejects(
    claimMount({
      dataRoot,
      pid: 2222,
      sessionIds: ['session-a'],
      isAlive: live([9999]),
      now: () => clock,
      sleep,
    }),
    (error) => error instanceof StartupError && error.code === 'startup-failed' && /busy/.test(error.message),
  )
})

test('an unreadable ledger refuses startup', async () => {
  const dataRoot = tempRoot()
  const ledgerPath = path.join(dataRoot, LEDGER_FILE)
  fs.writeFileSync(ledgerPath, JSON.stringify({ version: 1, owners: [] }))
  fs.chmodSync(ledgerPath, 0o000)
  try {
    await assert.rejects(
      claimMount({ dataRoot, pid: 1111, sessionIds: ['session-a'], isAlive: live([1111]) }),
      (error) => error instanceof StartupError && error.code === 'startup-failed' && Boolean(error.repairPath),
    )
  } finally {
    fs.chmodSync(ledgerPath, 0o600)
  }
})

test('only one of two concurrent claims for the same session succeeds', async () => {
  const dataRoot = tempRoot()
  const sleep = async (ms) => {
    await new Promise((resolve) => setTimeout(resolve, ms))
  }
  const results = await Promise.allSettled([
    claimMount({
      dataRoot,
      pid: 1111,
      sessionIds: ['session-a'],
      isAlive: live([1111, 2222]),
      sleep,
    }),
    claimMount({
      dataRoot,
      pid: 2222,
      sessionIds: ['session-a'],
      isAlive: live([1111, 2222]),
      sleep,
    }),
  ])
  const fulfilled = results.filter((result) => result.status === 'fulfilled')
  const rejected = results.filter((result) => result.status === 'rejected')
  assert.equal(fulfilled.length, 1)
  assert.equal(rejected.length, 1)
  assert.equal(rejected[0].reason.code, 'occupied')
  await releaseMount(fulfilled[0].value)
  assert.equal(readLedger(path.join(dataRoot, LEDGER_FILE)).owners.length, 0)
})
