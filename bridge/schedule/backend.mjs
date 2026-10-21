// Schedule storage isolation and official-service mounting.
//
// The schedule domain is routed to a second JSON backend (`nano-schedule-json`)
// whose root is the root session's own runtime-data storage. The route is
// installed by the launcher's runtime patch; this module registers the named
// backend, provides the service key storage-domain waits on, proves the route
// with a throwaway probe domain, and only then mounts the official
// `@deepseek-ai/dsh-schedule` service.
//
// Mounting by hand (instead of a static patch row) is what makes "do not mount
// the scheduler when storage is wrong" enforceable: the official service never
// opens its table until the probe has passed.
import fs from 'node:fs'
import path from 'node:path'
import { createRequire } from 'node:module'
import { pathToFileURL } from 'node:url'

import { StartupError } from '../runtime/errors.mjs'

/** Registered backend name for the schedule domain. */
export const SCHEDULE_BACKEND_NAME = 'nano-schedule-json'

/** Probe domain name; it must stay outside the `schedule` unit. */
export const SCHEDULE_PROBE_DOMAIN = 'nano_probe'

/** How long to wait for `ctx.storageDomain` to activate. */
export const STORAGE_DOMAIN_TIMEOUT_MS = 10_000

/** How long to wait for the mounted schedule service to appear. */
export const SCHEDULE_MOUNT_TIMEOUT_MS = 10_000

let officialModules

/**
 * Resolve the official packages from the dsh installation that launched this
 * host. The profile links the bridge by path, so a bare import here would walk
 * the repository's module path instead of the installation's; the launcher's
 * own entry point is the anchor Node uses for the official tree as well.
 * @returns {Promise<{JsonStorageBackend: Function, ScheduleService: Function,
 *   storageBackendServiceKey: (name: string) => string}>} official exports.
 */
export async function loadOfficialModules() {
  if (officialModules !== undefined) return officialModules
  const schedule = await import(resolveOfficial('@deepseek-ai/dsh-schedule'))
  const json = await import(resolveOfficial('@deepseek-ai/dsh-storage-json'))
  const storage = await import(resolveOfficial('@deepseek-ai/dsh-storage'))
  officialModules = {
    JsonStorageBackend: json.JsonStorageBackend,
    ScheduleService: schedule.default ?? schedule.ScheduleService,
    storageBackendServiceKey: storage.storageBackendServiceKey,
  }
  return officialModules
}

function resolveOfficial(specifier) {
  const anchor = process.argv[1]
  if (typeof anchor === 'string' && anchor !== '') {
    try {
      // `dsh` is commonly a symlink on PATH; resolve the real entry so Node
      // searches the installation's node_modules.
      return pathToFileURL(createRequire(fs.realpathSync(anchor)).resolve(specifier)).href
    } catch {
      // Fall through to the package-name anchor below.
    }
  }
  try {
    return pathToFileURL(createRequire(import.meta.url).resolve(specifier)).href
  } catch (error) {
    throw new StartupError('unsupported', `无法从当前 dsh 安装解析 ${specifier}：${describe(error)}`, { cause: error })
  }
}

/**
 * Register the schedule backend and expose the lifecycle service key that
 * storage-domain injects. Without `provide`, the domain facility stays pending
 * even though the backend is registered.
 * @param {object} ctx - host context carrying `storage`.
 * @param {string} root - backend root (the root session's storage directory).
 * @returns {Promise<object>} the registered backend instance.
 */
export async function installScheduleBackend(ctx, root) {
  const { JsonStorageBackend, storageBackendServiceKey } = await loadOfficialModules()
  const backend = new JsonStorageBackend(root)
  ctx.effect(() => {
    const unregister = ctx.storage.backend.register(SCHEDULE_BACKEND_NAME, backend)
    return async () => {
      unregister()
      await backend.close()
    }
  })
  ctx.provide(storageBackendServiceKey(SCHEDULE_BACKEND_NAME), backend)
  return backend
}

/**
 * Wait for the composed storage-domain facility, then prove that both the
 * `schedule` route and the probe route resolve to the schedule backend and that
 * a probe write lands in this root's storage.
 *
 * `facility.config` is read here on purpose: a probe domain can only prove its
 * own route, while the acceptance also requires the `schedule` route itself to
 * differ from the default `json` root. The read is validation only; all writes
 * stay inside the throwaway `nano_probe` unit.
 * @param {object} ctx - host context.
 * @param {{root: string, timeoutMs?: number}} options - probe inputs.
 * @returns {Promise<object>} the activated domain facility.
 */
export async function verifyScheduleStorage(ctx, { root, timeoutMs = STORAGE_DOMAIN_TIMEOUT_MS }) {
  const facility = await waitForService(ctx, 'storageDomain', timeoutMs, 'ctx.storageDomain')
  const config = facility.config
  if (config === null || typeof config !== 'object') {
    throw new StartupError('unsupported', 'storage-domain exposes no route configuration')
  }
  if (config.backend !== 'json') {
    throw new StartupError('unsupported', `storage-domain default backend is ${String(config.backend)}, expected json`)
  }
  const routes = config.routes ?? {}
  if (routes.schedule !== SCHEDULE_BACKEND_NAME) {
    throw new StartupError(
      'unsupported',
      `storage-domain routes.schedule is ${String(routes.schedule)}, expected ${SCHEDULE_BACKEND_NAME}`,
    )
  }
  if (routes[SCHEDULE_PROBE_DOMAIN] !== SCHEDULE_BACKEND_NAME) {
    throw new StartupError(
      'unsupported',
      `storage-domain routes.${SCHEDULE_PROBE_DOMAIN} is ${String(routes[SCHEDULE_PROBE_DOMAIN])}, expected ${SCHEDULE_BACKEND_NAME}`,
    )
  }
  if (!ctx.storage.backend.names().includes(SCHEDULE_BACKEND_NAME)) {
    throw new StartupError('unsupported', `${SCHEDULE_BACKEND_NAME} is not registered on the storage hub`)
  }

  await runProbe(facility, root)
  return facility
}

const PROBE_SPEC = Object.freeze({
  name: SCHEDULE_PROBE_DOMAIN,
  version: 1,
  tables: Object.freeze({
    records: Object.freeze({
      valueSchema: Object.freeze({
        parse(value) {
          if (value === null || typeof value !== 'object' || typeof value.probe !== 'string' || value.probe === '') {
            throw new Error('probe record must carry a non-empty probe string')
          }
          return value
        },
      }),
    }),
  }),
})

async function runProbe(facility, root) {
  const file = path.join(root, `${SCHEDULE_PROBE_DOMAIN}.json`)
  let domain
  try {
    domain = await facility.open(PROBE_SPEC)
  } catch (error) {
    throw new StartupError('unsupported', `调度探针域无法打开：${describe(error)}`, { cause: error })
  }
  const key = `probe-${process.pid}-${Date.now()}`
  try {
    await domain.table('records').put(key, { probe: key })
    let document
    try {
      document = JSON.parse(fs.readFileSync(file, 'utf8'))
    } catch (error) {
      throw new StartupError('unsupported', `调度探针未落在 ${file}：${describe(error)}`, { cause: error })
    }
    const stored = document?.tables?.records?.[key]
    if (document?.unit?.name !== SCHEDULE_PROBE_DOMAIN || stored?.probe !== key) {
      throw new StartupError('unsupported', `调度探针记录没有出现在 ${file}`)
    }
    await domain.table('records').delete(key)
  } catch (error) {
    if (error instanceof StartupError) throw error
    throw new StartupError('unsupported', `调度探针写入失败：${describe(error)}`, { cause: error })
  } finally {
    await domain.close().catch(() => {})
    try {
      fs.rmSync(file, { force: true })
    } catch {
      // The probe unit is disposable; a leftover empty file changes nothing.
    }
  }
}

/**
 * Mount the official schedule service after the storage probe passed.
 * @param {object} ctx - host context.
 * @param {{timeoutMs?: number}} [options] - mount timeout.
 * @returns {Promise<{fiber: object, service: object}>} mounted fiber and service.
 */
export async function mountScheduleService(ctx, { timeoutMs = SCHEDULE_MOUNT_TIMEOUT_MS } = {}) {
  const { ScheduleService } = await loadOfficialModules()
  if (typeof ScheduleService !== 'function') {
    throw new StartupError('unsupported', '官方 dsh-schedule 没有导出 ScheduleService')
  }
  const fiber = ctx.plugin(ScheduleService, {})
  try {
    const service = await waitForService(ctx, 'schedule', timeoutMs, 'ctx.schedule')
    // Reading the active catalog forces the service's own domain-open promise
    // to settle. A stale or unreadable schedule.json rejects here instead of
    // surfacing on the first user action.
    await service.catalog()
    return { fiber, service }
  } catch (error) {
    // A pending fiber must not hold startup open past the same bound. Its own
    // failure is already reported by the service load; disposal is cleanup.
    await Promise.race([fiber.dispose().catch(() => {}), delay(3000)])
    throw new StartupError('unsupported', `官方调度服务未挂载：${describe(error)}`, { cause: error })
  }
}

function delay(ms) {
  return new Promise((resolve) => setTimeout(resolve, ms))
}

/**
 * Wait for a Cordis service without polling.
 * @param {object} ctx - context to observe.
 * @param {string} name - service key.
 * @param {number} timeoutMs - bounded wait.
 * @param {string} label - diagnostic label.
 * @returns {Promise<object>} the service value.
 */
export function waitForService(ctx, name, timeoutMs, label) {
  const present = ctx.get(name)
  if (present !== undefined) return Promise.resolve(present)
  return new Promise((resolve, reject) => {
    const timer = setTimeout(() => {
      reject(new StartupError('unsupported', `${label} 在 ${String(timeoutMs)}ms 内没有激活`))
    }, timeoutMs)
    try {
      ctx.inject([name], (scoped) => {
        clearTimeout(timer)
        resolve(scoped.get(name))
      })
    } catch (error) {
      clearTimeout(timer)
      reject(new StartupError('unsupported', `${label} 注入失败：${describe(error)}`, { cause: error }))
    }
  })
}

function describe(error) {
  return error instanceof Error ? error.message : String(error)
}
