// The TextMate grammar, run by the engine VS Code itself uses (vscode-textmate with
// Oniguruma): a sample whose every scope is known, and every Staal file in the
// repository, each of which must end with nothing left open.

const test = require('node:test');
const assert = require('node:assert');
const fs = require('node:fs');
const path = require('node:path');
const tm = require('vscode-textmate');
const onig = require('vscode-oniguruma');

const ROOT = path.join(__dirname, '..', '..', '..');

async function grammar() {
  const wasm = fs.readFileSync(require.resolve('vscode-oniguruma/release/onig.wasm'));
  await onig.loadWASM(wasm.buffer.slice(wasm.byteOffset, wasm.byteOffset + wasm.byteLength));
  const registry = new tm.Registry({
    onigLib: Promise.resolve({ createOnigScanner: (p) => new onig.OnigScanner(p), createOnigString: (s) => new onig.OnigString(s) }),
    loadGrammar: async () =>
      tm.parseRawGrammar(fs.readFileSync(path.join(__dirname, '..', 'syntaxes', 'staal.tmLanguage.json'), 'utf8'), 'staal.tmLanguage.json'),
  });
  return registry.loadGrammar('source.staal');
}

// Whether `stack` has nothing open: it is the state after an empty first line.
const closed = (g, stack) => stack.equals(g.tokenizeLine('', tm.INITIAL).ruleStack);

// [text, innermost scope] for each token of `src`, and the rule stack at its end.
function tokens(g, src) {
  let stack = tm.INITIAL;
  const out = [];
  for (const line of src.split('\n')) {
    const r = g.tokenizeLine(line, stack);
    for (const t of r.tokens) {
      const text = line.slice(t.startIndex, t.endIndex);
      if (text.trim() !== '') out.push([text.trim(), t.scopes[t.scopes.length - 1]]);
    }
    stack = r.ruleStack;
  }
  return { out, stack };
}

test('a sample is scoped as written', async () => {
  const g = await grammar();
  const src = [
    'import sms.termii as sms',
    '// a comment with "quotes" and fn',
    'pub type Line { sku: Text, qty: Int, in: Bool }',
    'enum Shape { Dot }',
    'fn Line.cost<U>(self, p: cap payments.refund(max: 50 USD), @blind note: Text) -> Money {',
    '    let t = "a \\(len(x) + 1) b\\n\\q"',
    '    if order.type == true and not done { return 12.50 NGN }',
    '    for i in 1..n { set total = total + f(i) }',
    '    let block = """',
    '        two // not a comment',
    '        """',
    '}',
  ].join('\n');
  const { out, stack } = tokens(g, src);
  const scope = (text, nth = 0) => out.filter(([t]) => t === text)[nth]?.[1];
  const want = {
    import: 'keyword.other.staal',
    '// a comment with "quotes" and fn': 'comment.line.double-slash.staal',
    pub: 'storage.modifier.staal',
    type: 'storage.type.staal',
    Line: 'entity.name.type.staal',
    Text: 'support.type.staal',
    in: 'variable.other.property.staal',
    enum: 'storage.type.staal',
    Shape: 'entity.name.type.staal',
    fn: 'storage.type.function.staal',
    cost: 'entity.name.function.staal',
    self: 'variable.language.self.staal',
    cap: 'keyword.other.staal',
    '50': 'constant.numeric.staal',
    USD: 'support.constant.currency.staal',
    '@blind': 'storage.modifier.mark.staal',
    '->': 'keyword.operator.staal',
    let: 'keyword.other.staal',
    '\\(': 'punctuation.section.interpolation.begin.staal',
    len: 'support.function.builtin.staal',
    '\\n': 'constant.character.escape.staal',
    '\\q': 'invalid.illegal.escape.staal',
    if: 'keyword.control.staal',
    true: 'constant.language.staal',
    and: 'keyword.operator.word.staal',
    not: 'keyword.operator.word.staal',
    done: 'keyword.control.staal',
    '12.50': 'constant.numeric.staal',
    NGN: 'support.constant.currency.staal',
    '..': 'keyword.operator.staal',
    f: 'entity.name.function.call.staal',
    'two // not a comment': 'string.quoted.triple.staal',
  };
  for (const [text, s] of Object.entries(want)) assert.strictEqual(scope(text), s, `${text}: ${JSON.stringify(out)}`);
  // `type` after a dot is a field; the second `in` is the loop's keyword.
  assert.strictEqual(scope('type', 1), 'variable.other.property.staal');
  assert.strictEqual(scope('in', 1), 'keyword.control.staal');
  // The interpolation closes at its own parenthesis, not at len's.
  const close = out.findIndex(([t], i) => t === ')' && out[i - 1][0] === '1');
  assert.strictEqual(out[close][1], 'punctuation.section.interpolation.end.staal', JSON.stringify(out));
  assert.strictEqual(out[close - 3][1], 'meta.interpolation.staal');
  assert.ok(closed(g, stack), 'something is left open at the end');
  assert.ok(!closed(g, tokens(g, 'let t = """\n  open').stack), 'an open text block is seen');
  // A one-line text never crosses a line break, even one left unclosed.
  const after = tokens(g, 'let t = "open\nlet n = 1');
  assert.deepStrictEqual(after.out.slice(-4), [['let', 'keyword.other.staal'], ['n', 'source.staal'], ['=', 'keyword.operator.staal'], ['1', 'constant.numeric.staal']]);
  assert.ok(closed(g, after.stack));
});

test('every Staal file in the repository ends with nothing open', async () => {
  const g = await grammar();
  const files = [];
  const walk = (d) => {
    for (const e of fs.readdirSync(d, { withFileTypes: true })) {
      const p = path.join(d, e.name);
      // Fuzz corpora and findings are copies and mutations, never committed.
      const fuzzData = path.basename(d) === 'fuzz' && ['corpus', 'artifacts'].includes(e.name);
      if (e.isDirectory() && !['target', 'node_modules', '.git'].includes(e.name) && !fuzzData) walk(p);
      else if (e.isFile() && e.name.endsWith('.staal')) files.push(p);
    }
  };
  walk(ROOT);
  assert.ok(files.length > 700, `only ${files.length} files`);
  const open = files.filter((f) => !closed(g, tokens(g, fs.readFileSync(f, 'utf8')).stack));
  // Only these end open, as they should: each is refused for a parenthesis it never
  // closes.
  assert.deepStrictEqual(open.map((f) => path.relative(ROOT, f)).sort(), [
    'tests/fixtures/fail/e901_syntax.staal',
    'tests/modules/e901_error_in_module/lib.staal',
  ]);
});
