// Profile installer tests. They use --no-install so no pnpm or dsh process is
// needed; the real install path is covered by the Rust E2E tests.
import test from 'node:test'
import assert from 'node:assert/strict'
import fs from 'node:fs'
import os from 'node:os'
import path from 'node:path'
import { spawnSync } from 'node:child_process'
import { fileURLToPath } from 'node:url'

const installScript = fileURLToPath(new URL('../profile/install.mjs', import.meta.url))
const bridgePackage = fileURLToPath(new URL('..', import.meta.url))

function run(args) {
  return spawnSync(process.execPath, [installScript, ...args], { encoding: 'utf8' })
}

function tempRoot() {
  return fs.mkdtempSync(path.join(os.tmpdir(), 'nano-profile-'))
}

test('initializes an idempotent profile with the protocol dependency', () => {
  const root = tempRoot()
  const dshHome = path.join(root, 'dsh-home')
  const dataRoot = path.join(root, 'data')
  const args = [
    '--dsh-home',
    dshHome,
    '--data-root',
    dataRoot,
    '--bridge-package',
    bridgePackage,
    '--no-install',
    '--json',
  ]
  const first = run(args)
  assert.equal(first.status, 0, first.stderr)
  const summary = JSON.parse(first.stdout.trim())
  assert.equal(summary.profileDir, path.join(dshHome, 'profiles', 'dsh-tui-nano'))
  assert.equal(summary.installed, false)

  const manifestPath = path.join(summary.profileDir, 'package.json')
  const manifest = JSON.parse(fs.readFileSync(manifestPath, 'utf8'))
  assert.equal(manifest.name, 'dsh-profile-dsh-tui-nano')
  assert.deepEqual(manifest.dsh.profile.bundles, ['@deepseek-ai/dsh-base', '@dsh-tui-nano/bridge'])
  assert.ok(manifest.dependencies['@dsh-tui-nano/bridge'].startsWith('file:'))
  assert.ok(manifest.dependencies['@dsh-tui-nano/protocol'].startsWith('file:'))
  assert.equal(fs.readFileSync(path.join(summary.profileDir, 'cordis.yml'), 'utf8'), '[]\n')
  assert.equal(fs.readFileSync(path.join(summary.profileDir, 'cordis.patch.yml'), 'utf8'), '[]\n')
  assert.equal(fs.statSync(dataRoot).mode & 0o777, 0o700)
  assert.equal(fs.statSync(manifestPath).mode & 0o777, 0o600)

  // A user patch and a second run must both survive.
  const patchPath = path.join(summary.profileDir, 'cordis.patch.yml')
  fs.writeFileSync(patchPath, '# user patch\n[]\n')
  const second = run(args)
  assert.equal(second.status, 0, second.stderr)
  assert.equal(fs.readFileSync(patchPath, 'utf8'), '# user patch\n[]\n')
  assert.deepEqual(
    JSON.parse(fs.readFileSync(manifestPath, 'utf8')).dsh.profile.bundles,
    manifest.dsh.profile.bundles,
  )
})

test('refuses to take over a profile owned by another product', () => {
  const root = tempRoot()
  const dshHome = path.join(root, 'dsh-home')
  const profileDir = path.join(dshHome, 'profiles', 'dsh-tui-nano')
  fs.mkdirSync(profileDir, { recursive: true })
  fs.writeFileSync(
    path.join(profileDir, 'package.json'),
    JSON.stringify({ name: 'someone-else', private: true }),
  )
  const result = run([
    '--dsh-home',
    dshHome,
    '--data-root',
    path.join(root, 'data'),
    '--bridge-package',
    bridgePackage,
    '--no-install',
  ])
  assert.equal(result.status, 2)
  assert.match(result.stderr, /belongs to another product/)
})

test('refuses a bridge package without the protocol sibling', () => {
  const root = tempRoot()
  const isolatedBridge = path.join(root, 'isolated', 'bridge')
  fs.mkdirSync(isolatedBridge, { recursive: true })
  fs.writeFileSync(path.join(isolatedBridge, 'package.json'), JSON.stringify({ name: '@dsh-tui-nano/bridge' }))
  const result = run([
    '--dsh-home',
    path.join(root, 'dsh-home'),
    '--data-root',
    path.join(root, 'data'),
    '--bridge-package',
    isolatedBridge,
    '--no-install',
  ])
  assert.equal(result.status, 2)
  assert.match(result.stderr, /protocol package is missing/)
})
