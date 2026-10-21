// Canonical method and notification names implemented by this host generation.
// Tests compare these sets with protocol/nano-protocol.json so neither side can
// silently drift from the contract.

/** Methods that receive a `nano.*` request frame. */
export const IMPLEMENTED_REQUEST_METHODS = Object.freeze([
  'nano.shutdown',
  'nano.subscribe',
  'nano.unsubscribe',
  'nano.cancelRequest',
  'nano.conversation.submit',
  'nano.conversation.editQueued',
  'nano.conversation.deleteQueued',
  'nano.conversation.steerQueued',
  'nano.conversation.cancel',
  'nano.conversation.snapshot',
  'nano.terminal.environment',
  'nano.terminal.shells',
  'nano.terminal.list',
  'nano.terminal.create',
  'nano.terminal.retain',
  'nano.terminal.release',
  'nano.terminal.attach',
  'nano.terminal.detach',
  'nano.terminal.rename',
  'nano.terminal.close',
])

/** Notifications this host can emit. */
export const IMPLEMENTED_NOTIFICATION_METHODS = Object.freeze([
  'nano.hello',
  'nano.startupError',
  'nano.event',
  'nano.streamEnded',
  'nano.terminal.output',
  'nano.terminal.state',
  'nano.terminal.ack',
  'nano.terminal.ended',
])

/** Client-to-host notifications. */
export const IMPLEMENTED_CLIENT_NOTIFICATION_METHODS = Object.freeze([
  'nano.terminal.input',
  'nano.terminal.resize',
])

/** Every implemented method name. */
export const IMPLEMENTED_METHODS = Object.freeze([
  ...IMPLEMENTED_REQUEST_METHODS,
  ...IMPLEMENTED_NOTIFICATION_METHODS,
  ...IMPLEMENTED_CLIENT_NOTIFICATION_METHODS,
])
