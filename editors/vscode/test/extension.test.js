// extension.js with VS Code and its language client replaced by recorders: what it
// starts, what it says when it cannot, and that a new server path restarts it.

const test = require('node:test');
const assert = require('node:assert');
const Module = require('node:module');

function load({ failStart = false } = {}) {
  const log = { clients: [], errors: [], listeners: [], path: undefined };
  const vscode = {
    workspace: {
      getConfiguration: (section) => ({ get: (key) => (section === 'staal' && key === 'server.path' ? log.path : undefined) }),
      onDidChangeConfiguration: (f) => {
        log.listeners.push(f);
        return { dispose() {} };
      },
    },
    window: { showErrorMessage: (m) => log.errors.push(m) },
  };
  class LanguageClient {
    constructor(id, name, server, options) {
      Object.assign(this, { id, name, server, options, started: false, stopped: false });
      log.clients.push(this);
    }
    async start() {
      if (failStart) throw new Error('spawn staalc ENOENT');
      this.started = true;
    }
    async stop() {
      this.stopped = true;
    }
  }
  const real = Module._load;
  Module._load = function (request, ...rest) {
    if (request === 'vscode') return vscode;
    if (request === 'vscode-languageclient/node') return { LanguageClient };
    return real.call(this, request, ...rest);
  };
  try {
    delete require.cache[require.resolve('../extension.js')];
    return { ext: require('../extension.js'), log };
  } finally {
    Module._load = real;
  }
}

test('it starts `staalc lsp` for Staal files and unsaved documents', async () => {
  const { ext, log } = load();
  const context = { subscriptions: [] };
  await ext.activate(context);
  assert.strictEqual(log.clients.length, 1);
  const c = log.clients[0];
  assert.deepStrictEqual(c.server, { command: 'staalc', args: ['lsp'] });
  assert.deepStrictEqual(c.options.documentSelector, [
    { language: 'staal', scheme: 'file' },
    { language: 'staal', scheme: 'untitled' },
  ]);
  assert.ok(c.started);
  assert.strictEqual(context.subscriptions.length, 1);

  // A new path restarts the server with it; an unrelated change does nothing.
  log.path = '  /opt/staal/staalc ';
  await log.listeners[0]({ affectsConfiguration: (s) => s === 'editor.fontSize' });
  assert.strictEqual(log.clients.length, 1);
  await log.listeners[0]({ affectsConfiguration: (s) => s === 'staal.server.path' });
  assert.ok(c.stopped);
  assert.strictEqual(log.clients.length, 2);
  assert.strictEqual(log.clients[1].server.command, '/opt/staal/staalc');

  await ext.deactivate();
  assert.ok(log.clients[1].stopped);
  assert.deepStrictEqual(log.errors, []);
});

test('a server that cannot start is reported, and deactivating is then safe', async () => {
  const { ext, log } = load({ failStart: true });
  await ext.activate({ subscriptions: [] });
  assert.strictEqual(log.errors.length, 1);
  assert.match(log.errors[0], /could not start `staalc lsp` \(spawn staalc ENOENT\)/);
  assert.match(log.errors[0], /staal\.server\.path/);
  await ext.deactivate();
  assert.strictEqual(log.clients[0].stopped, false);
});
