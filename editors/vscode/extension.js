// The Staal extension: the grammar in syntaxes/ highlights; everything else (errors as
// you type, completion, formatting) comes from `staalc lsp`, so the editor reports
// exactly what `staalc check` reports.

const vscode = require('vscode');
const { LanguageClient } = require('vscode-languageclient/node');

let client;

function serverPath() {
  const p = vscode.workspace.getConfiguration('staal').get('server.path');
  return typeof p === 'string' && p.trim() !== '' ? p.trim() : 'staalc';
}

async function start() {
  const command = serverPath();
  const next = new LanguageClient(
    'staal',
    'Staal',
    { command, args: ['lsp'] },
    { documentSelector: [{ language: 'staal', scheme: 'file' }, { language: 'staal', scheme: 'untitled' }] },
  );
  try {
    await next.start();
    client = next;
  } catch (e) {
    client = undefined;
    vscode.window.showErrorMessage(
      `Staal: could not start \`${command} lsp\` (${e && e.message ? e.message : e}). ` +
        'Set "staal.server.path" to the staalc executable.',
    );
  }
}

async function stop() {
  const c = client;
  client = undefined;
  if (c) {
    await c.stop();
  }
}

async function activate(context) {
  context.subscriptions.push(
    vscode.workspace.onDidChangeConfiguration(async (e) => {
      if (e.affectsConfiguration('staal.server.path')) {
        await stop();
        await start();
      }
    }),
  );
  await start();
}

function deactivate() {
  return stop();
}

module.exports = { activate, deactivate };
