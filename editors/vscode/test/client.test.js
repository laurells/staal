// `staalc lsp` driven by the protocol library VS Code's client is built on
// (vscode-languageserver-protocol), so the framing and messages are the ones a real
// editor sends. The server is STAALC, or the repository's debug build.

const test = require('node:test');
const assert = require('node:assert');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const { spawn } = require('node:child_process');
const { pathToFileURL } = require('node:url');
const rpc = require('vscode-languageserver-protocol/node');

const BIN = process.env.STAALC || path.join(__dirname, '..', '..', '..', 'target', 'debug', 'staalc');

test('the server works with the protocol library', { timeout: 60000 }, async () => {
  assert.ok(fs.existsSync(BIN), `no staalc at ${BIN}: build it, or set STAALC`);
  const child = spawn(BIN, ['lsp'], { stdio: ['pipe', 'pipe', 'inherit'] });
  const exited = new Promise((resolve) => child.on('exit', (code) => resolve(code)));
  const conn = rpc.createProtocolConnection(new rpc.StreamMessageReader(child.stdout), new rpc.StreamMessageWriter(child.stdin));
  const published = [];
  let seen = () => {};
  conn.onNotification(rpc.PublishDiagnosticsNotification.type, (p) => {
    published.push(p);
    seen();
  });
  conn.listen();

  const init = await conn.sendRequest(rpc.InitializeRequest.type, { processId: process.pid, rootUri: null, capabilities: {} });
  assert.strictEqual(init.capabilities.textDocumentSync.change, rpc.TextDocumentSyncKind.Full);
  await conn.sendNotification(rpc.InitializedNotification.type, {});

  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'staal-client-'));
  try {
    const file = path.join(dir, 'a.staal');
    const text = 'fn f() -> Int {\n    return y\n}\n';
    fs.writeFileSync(file, text);
    const uri = pathToFileURL(file).toString();
    const got = new Promise((resolve) => (seen = () => published.some((p) => p.uri === uri) && resolve()));
    await conn.sendNotification(rpc.DidOpenTextDocumentNotification.type, { textDocument: { uri, languageId: 'staal', version: 1, text } });
    await got;
    const d = published.find((p) => p.uri === uri).diagnostics;
    assert.deepStrictEqual(
      d.map((x) => [x.range.start, x.code, x.message, x.severity, x.source]),
      [[{ line: 1, character: 11 }, 'E100', "undefined name 'y'", rpc.DiagnosticSeverity.Error, 'staalc']],
    );

    const items = await conn.sendRequest(rpc.CompletionRequest.type, { textDocument: { uri }, position: { line: 1, character: 11 } });
    assert.ok(items.some((i) => i.label === 'validate' && i.kind === rpc.CompletionItemKind.Keyword));
    assert.ok(items.some((i) => i.label === 'f' && i.kind === rpc.CompletionItemKind.Function));

    const edits = await conn.sendRequest(rpc.DocumentFormattingRequest.type, { textDocument: { uri }, options: { tabSize: 4, insertSpaces: true } });
    assert.deepStrictEqual(edits, []);

    await conn.sendRequest(rpc.ShutdownRequest.type);
    await conn.sendNotification(rpc.ExitNotification.type);
    assert.strictEqual(await exited, 0);
  } finally {
    conn.dispose();
    child.kill();
    fs.rmSync(dir, { recursive: true, force: true });
  }
});
