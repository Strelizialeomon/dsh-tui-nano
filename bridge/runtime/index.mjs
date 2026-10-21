// Cordis entry for the Nano host. The Cordis loader mounts this row last in the
// Nano profile; all startup work is detached because the loader must finish
// mounting the rest of the tree before the host may create its root session.
import { startHost } from './host.mjs'

export const name = 'nano-bridge'

export const inject = ['agents', 'sessions', 'sessionPersistence', 'storage']

/**
 * Mount the host.
 * @param {object} ctx - Cordis context.
 * @param {object} config - runtime config from the launcher-supplied patch.
 * @returns {void} startup continues asynchronously.
 */
export function apply(ctx, config) {
  startHost(ctx, config)
}
