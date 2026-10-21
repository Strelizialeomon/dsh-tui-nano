#!/usr/bin/env node
// Initialize the Nano dsh profile and install the linked bridge package.
//
// This script is idempotent. It never overwrites a user's cordis.patch.yml and
// refuses to take over a profile directory that belongs to another product.
import fs from 'node:fs'
import path from 'node:path'
import { fileURLToPath } from 'node:url'
import { execFileSync, spawnSync } from 'node:child_process'
import { parseArgs } from 'node:util'

const here = path.dirname(fileURLToPath(import.meta.url))
const packageRoot = path.resolve(here, '..')
const repoRoot = path.resolve(packageRoot, '..')
const protocolFile = path.join(repoRoot, 'protocol', 'nano-protocol.json')
const BRIDGE_PACKAGE = '@dsh-tui-nano/bridge'
const DEFAULT_PROFILE = 'dsh-tui-nano'

const { values } = parseArgs({
  options: {
    'dsh-home': { type: 'string' },
    'data-root': { type: 'string' },
    'bridge-package': { type: 'string' },
    profile: { type: 'string', default: DEFAULT_PROFILE },
    'dsh-bin': { type: 'string' },
    'no-install': { type: 'boolean', default: false },
    json: { type: 'boolean', default: false },
  },
})

try {
  const summary = install({
    dshHome: values['dsh-home'],
    dataRoot: values['data-root'],
    bridgePackage: values['bridge-package'] ?? packageRoot,
    profile: values.profile,
    dshBin: values['dsh-bin'],
    noInstall: values['no-install'],
  })
  if (values.json) process.stdout.write(`${JSON.stringify(summary)}\n`)
  else {
    process.stdout.write(`profile: ${summary.profileDir}\n`)
    process.stdout.write(`data root: ${summary.dataRoot}\n`)
    process.stdout.write(`bridge package: ${summary.bridgePackage}\n`)
    process.stdout.write(`installed: ${summary.installed ? 'yes' : 'no'}\n`)
  }
} catch (error) {
  process.stderr.write(`nano-profile-install: ${error instanceof Error ? error.message : String(error)}\n`)
  process.exitCode = 2
}

/**
 * @param {{dshHome?: string, dataRoot?: string, bridgePackage: string, profile: string,
 *   dshBin?: string, noInstall: boolean}} options - installer inputs.
 * @returns {{ok: true, dshHome: string, profileDir: string, dataRoot: string, bridgePackage: string, installed: boolean}}
 */
function install({ dshHome, dataRoot, bridgePackage, profile, dshBin, noInstall }) {
  const resolvedBridge = path.resolve(bridgePackage)
  const resolvedProtocol = path.resolve(path.dirname(resolvedBridge), 'protocol')
  if (!fs.existsSync(path.join(resolvedProtocol, 'package.json'))) {
    throw new Error(`protocol package is missing package.json: ${resolvedProtocol}`)
  }
  if (!fs.existsSync(path.join(resolvedBridge, 'package.json'))) {
    throw new Error(`bridge package is missing package.json: ${resolvedBridge}`)
  }
  if (!fs.existsSync(protocolFile)) {
    throw new Error(`protocol contract is missing: ${protocolFile}`)
  }
  const resolvedDshHome = path.resolve(dshHome ?? process.env.DSH_HOME ?? path.join(homeDirectory(), '.dsh'))
  const resolvedDataRoot = path.resolve(dataRoot ?? path.join(homeDirectory(), '.dsh-tui-nano'))
  const profilesRoot = path.join(resolvedDshHome, 'profiles')
  const profileDir = path.join(profilesRoot, profile)
  fs.mkdirSync(profileDir, { recursive: true, mode: 0o700 })
  fs.chmodSync(profileDir, 0o700)
  fs.mkdirSync(resolvedDataRoot, { recursive: true, mode: 0o700 })
  fs.chmodSync(resolvedDataRoot, 0o700)

  writeProfileManifest({ profileDir, profile, bridgePackage: resolvedBridge, protocolPackage: resolvedProtocol })
  writeIfMissing(path.join(profileDir, 'cordis.yml'), '[]\n')
  writeIfMissing(path.join(profileDir, 'cordis.patch.yml'), '[]\n')
  writeIfMissing(
    path.join(profileDir, 'pnpm-workspace.yaml'),
    'packages:\n  - .\n\nnodeLinker: hoisted\nautoInstallPeers: false\n',
  )

  let installed = false
  if (!noInstall) {
    const dsh = resolveDshBin(dshBin)
    runChecked(dsh, ['plugin', '--profile', profile, 'install'], {
      cwd: profileDir,
      env: { ...process.env, DSH_HOME: resolvedDshHome },
      label: 'dsh plugin install',
    })
    const dump = runChecked(dsh, ['--profile', profile, '--dump-config'], {
      cwd: profileDir,
      env: { ...process.env, DSH_HOME: resolvedDshHome },
      label: 'dsh --dump-config',
    })
    if (!dump.includes('nano-bridge') || !dump.includes(BRIDGE_PACKAGE)) {
      throw new Error('composed profile does not contain the nano-bridge row after install')
    }
    installed = true
  }

  return {
    ok: true,
    dshHome: resolvedDshHome,
    profileDir,
    dataRoot: resolvedDataRoot,
    bridgePackage: resolvedBridge,
    installed,
  }
}

function writeProfileManifest({ profileDir, profile, bridgePackage, protocolPackage }) {
  const manifestPath = path.join(profileDir, 'package.json')
  let manifest
  if (fs.existsSync(manifestPath)) {
    try {
      manifest = JSON.parse(fs.readFileSync(manifestPath, 'utf8'))
    } catch (error) {
      throw new Error(`profile package.json is not valid JSON: ${manifestPath}`, { cause: error })
    }
    const owned =
      manifest?.name === `dsh-profile-${profile}` ||
      manifest?.dependencies?.[BRIDGE_PACKAGE] !== undefined
    if (!owned) {
      throw new Error(`profile ${profileDir} belongs to another product (${String(manifest?.name)}); refusing to overwrite it`)
    }
  }
  const next = {
    ...manifest,
    name: `dsh-profile-${profile}`,
    private: true,
    dependencies: {
      ...(manifest?.dependencies ?? {}),
      [BRIDGE_PACKAGE]: `file:${bridgePackage}`,
      '@dsh-tui-nano/protocol': `file:${protocolPackage}`,
    },
    dsh: {
      ...(manifest?.dsh ?? {}),
      profile: {
        ...(manifest?.dsh?.profile ?? {}),
        bundles: ['@deepseek-ai/dsh-base', BRIDGE_PACKAGE],
      },
    },
  }
  fs.writeFileSync(manifestPath, `${JSON.stringify(next, null, 2)}\n`, { mode: 0o600 })
}

function writeIfMissing(target, content) {
  if (!fs.existsSync(target)) fs.writeFileSync(target, content, { mode: 0o600 })
}

function resolveDshBin(explicit) {
  if (explicit !== undefined) return path.resolve(explicit)
  try {
    return execFileSync('which', ['dsh'], { encoding: 'utf8' }).trim()
  } catch {
    const besideNode = path.join(path.dirname(process.execPath), 'dsh')
    if (fs.existsSync(besideNode)) return besideNode
    throw new Error('dsh was not found on PATH; pass --dsh-bin')
  }
}

function runChecked(command, args, { cwd, env, label }) {
  const result = spawnSync(command, args, {
    cwd,
    env,
    encoding: 'utf8',
    maxBuffer: 16 * 1024 * 1024,
  })
  if (result.error !== undefined) throw new Error(`${label} failed to start: ${result.error.message}`)
  if (result.status !== 0) {
    const tail = `${result.stderr ?? ''}\n${result.stdout ?? ''}`.trim().split('\n').slice(-20).join('\n')
    throw new Error(`${label} exited ${String(result.status)}:\n${tail}`)
  }
  return `${result.stdout ?? ''}${result.stderr ?? ''}`
}

function homeDirectory() {
  return process.env.HOME ?? execFileSync('sh', ['-c', 'cd ~ && pwd'], { encoding: 'utf8' }).trim()
}
