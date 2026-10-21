// Canonical method and notification names implemented by this host generation.
// Tests compare these sets with protocol/nano-protocol.json so neither side can
// silently drift from the contract.

/** Methods that receive a `nano.*` request frame. */
export const IMPLEMENTED_REQUEST_METHODS = Object.freeze([
  'nano.shutdown',
  'nano.subscribe',
  'nano.unsubscribe',
  'nano.cancelRequest',
  'nano.attachment.begin',
  'nano.attachment.chunk',
  'nano.attachment.finish',
  'nano.attachment.discard',
  'nano.fileReferences.list',
  'nano.conversation.submit',
  'nano.conversation.editQueued',
  'nano.conversation.deleteQueued',
  'nano.conversation.steerQueued',
  'nano.conversation.cancel',
  'nano.conversation.snapshot',
  'nano.shell.exec',
  'nano.jobs.snapshot',
  'nano.jobs.follow',
  'nano.jobs.kill',
  'nano.skills.list',
  'nano.skills.preview',
  'nano.instructions.snapshot',
  'nano.controls.state',
  'nano.controls.modelCatalog',
  'nano.controls.selectModel',
  'nano.controls.setPermission',
  'nano.controls.executeCommand',
])

/** Notifications this host can emit. */
export const IMPLEMENTED_NOTIFICATION_METHODS = Object.freeze([
  'nano.hello',
  'nano.startupError',
  'nano.event',
  'nano.streamEnded',
])

/** Every implemented method name. */
export const IMPLEMENTED_METHODS = Object.freeze([
  ...IMPLEMENTED_REQUEST_METHODS,
  ...IMPLEMENTED_NOTIFICATION_METHODS,
])
