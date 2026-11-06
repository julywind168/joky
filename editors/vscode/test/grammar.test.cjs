const assert = require('node:assert/strict');
const { readFileSync, readdirSync } = require('node:fs');
const path = require('node:path');
const { before, test } = require('node:test');
const { Registry, INITIAL, parseRawGrammar } = require('vscode-textmate');
const { loadWASM, OnigScanner, OnigString } = require('vscode-oniguruma');

const root = path.resolve(__dirname, '..');
const manifest = JSON.parse(readFileSync(path.join(root, 'package.json'), 'utf8'));
const configuration = JSON.parse(readFileSync(path.join(root, 'language-configuration.json'), 'utf8'));
let grammar;
let registry;
before(async () => {
  const wasm = readFileSync(require.resolve('vscode-oniguruma/release/onig.wasm'));
  await loadWASM(wasm.buffer.slice(wasm.byteOffset, wasm.byteOffset + wasm.byteLength));
  registry = new Registry({
    onigLib: Promise.resolve({
      createOnigScanner: (patterns) => new OnigScanner(patterns),
      createOnigString: (text) => new OnigString(text),
    }),
    loadGrammar: async () => parseRawGrammar(
      readFileSync(path.join(root, manifest.contributes.grammars[0].path), 'utf8'),
      'joky.tmLanguage.json',
    ),
  });
  grammar = await registry.loadGrammarWithConfiguration('source.joky', 1, {
    embeddedLanguages: { 'meta.embedded.inline.joky': 1 },
  });
});

function tokenize(source) {
  let state = INITIAL;
  const lines = source.split(/\r\n|\r|\n/).map((line) => {
    const result = grammar.tokenizeLine(line, state);
    assert.equal(result.stoppedEarly, false);
    state = result.ruleStack;
    return { line, tokens: result.tokens };
  });
  return { lines, state };
}
function scopes(result, row, text, occurrence = 0) {
  const { line, tokens } = result.lines[row];
  let position = -1;
  for (let i = 0; i <= occurrence; i++) position = line.indexOf(text, position + 1);
  assert.notEqual(position, -1, `Missing ${text} on line ${row + 1}`);
  return tokens.find((t) => t.startIndex <= position && t.endIndex > position).scopes;
}
function has(result, row, text, scope, occurrence = 0) {
  assert.ok(scopes(result, row, text, occurrence).includes(`${scope}.joky`),
    `${text}: expected ${scope}, got ${scopes(result, row, text, occurrence)}`);
}
function closed(result) {
  assert.ok(result.state.equals(grammar.tokenizeLine('', INITIAL).ruleStack), `Unclosed grammar state: ${result.state}`);
}

test('manifest registers a declarative .jk language with embedded interpolation', () => {
  assert.deepEqual(manifest.contributes.languages[0].extensions, ['.jk']);
  assert.equal(manifest.contributes.grammars[0].scopeName, 'source.joky');
  assert.equal(manifest.main, undefined);
  assert.equal(manifest.contributes.grammars[0].embeddedLanguages['meta.embedded.inline.joky'], 'joky');
});

test('declarations, effects, types, Unicode identifiers and keyword boundaries', () => {
  const result = tokenize('pub fn 计算(value: UInt64) @suspends -> Bool { let gift = true; gift }\n#derive(Clone)\nstruct Point { x: Float32 }');
  has(result, 0, 'pub', 'storage.modifier');
  has(result, 0, '计算', 'entity.name.function');
  has(result, 0, 'UInt64', 'support.type');
  has(result, 0, '@suspends', 'storage.type.annotation');
  has(result, 0, 'true', 'constant.language');
  has(result, 0, 'gift', 'variable.other');
  has(result, 1, '#derive', 'storage.type.annotation');
  has(result, 2, 'Point', 'entity.name.type');
  closed(result);
});

test('all lexer keywords and builtin types have explicit scopes', () => {
  // Detect lexer additions so the grammar does not silently drift behind Joky.
  const lexer = readFileSync(path.join(root, '../../src/core/parser/lexer.rs'), 'utf8');
  const words = [...lexer.matchAll(/"(\w+)" => TokenKind::\w+,/g)].map((match) => match[1]);
  assert.ok(words.length > 40);
  for (const word of words) {
    const tokenScopes = scopes(tokenize(word), 0, word);
    assert.ok(tokenScopes.some((scope) => /^(keyword|storage|constant\.language|support\.type|invalid\.deprecated)\./.test(scope)), word);
  }
});

test('nested block comments ignore quotes and interpolation and restore code', () => {
  const result = tokenize('/* outer " { //\n /* nested */ still comment\n*/ fn after() {} // tail\nlet n = 1');
  has(result, 1, 'still', 'comment.block');
  has(result, 2, 'after', 'entity.name.function');
  has(result, 2, 'tail', 'comment.line.double-slash');
  has(result, 3, 'let', 'storage.type');
  closed(result);
});

test('strings distinguish escapes, literal braces and interpolation', () => {
  const result = tokenize(String.raw`let text = "hello {{name}} \u{4E2D} \n {value} // /*"; let after = 1`);
  has(result, 0, '{{', 'constant.character.escape');
  has(result, 0, '\\u{4E2D}', 'constant.character.escape');
  has(result, 0, 'value', 'meta.embedded.inline');
  has(result, 0, '//', 'string.quoted.double');
  has(result, 0, 'after', 'variable.other');
  closed(result);
});

test('interpolation balances records, nested strings and comments across lines', () => {
  const result = tokenize('let text = "before {\n  /* } */ if true { "inner {value}" } else { r#"}"# }\n} after"\nlet done = 1');
  has(result, 1, 'if', 'keyword.control');
  has(result, 1, 'inner', 'string.quoted.double');
  has(result, 1, 'value', 'variable.other');
  has(result, 2, 'after', 'string.quoted.double');
  has(result, 3, 'let', 'storage.type');
  closed(result);
});

test('embedded code resets the TextMate string token type for editor features', () => {
  const line = '"before {value + 1} after"';
  const { tokens } = grammar.tokenizeLine2(line, INITIAL);
  const typeAt = (offset) => {
    let metadata;
    for (let i = 0; i < tokens.length && tokens[i] <= offset; i += 2) metadata = tokens[i + 1];
    return (metadata >>> 8) & 3;
  };
  assert.equal(typeAt(line.indexOf('before')), 2); // String.
  assert.equal(typeAt(line.indexOf('value')), 0); // Other / code.
  assert.equal(typeAt(line.indexOf('after')), 2);
});

test('SQL templates distinguish parameter and identifier holes from ordinary interpolation', () => {
  const line = 'let q = sql"SELECT ${customer}, {postgres.ident("name")!}, \'{{}}\', \\u{24}"; let after = 1';
  const result = tokenize(line);
  has(result, 0, 'sql', 'storage.type.string.sql');
  has(result, 0, 'SELECT', 'string.quoted.double.sql');
  has(result, 0, '${', 'punctuation.section.interpolation.parameter.begin');
  // SQL-specific scopes keep the distinction available to themes.
  has(result, 0, 'customer', 'meta.interpolation.parameter.sql');
  has(result, 0, 'customer', 'meta.embedded.inline');
  has(result, 0, 'postgres', 'meta.interpolation.identifier.sql');
  has(result, 0, 'name', 'string.quoted.double');
  has(result, 0, '{{', 'constant.character.escape');
  has(result, 0, '\\u{24}', 'constant.character.escape');
  has(result, 0, 'after', 'variable.other');
  closed(result);

  const ordinary = tokenize('let sql = "${customer}"; let mysql = r"${customer}"');
  has(ordinary, 0, 'sql', 'variable.other');
  has(ordinary, 0, 'customer', 'meta.embedded.inline');
  assert.ok(!scopes(ordinary, 0, 'customer').includes('meta.interpolation.parameter.sql.joky'));
  has(ordinary, 0, 'customer', 'string.quoted.double.raw', 1);
  closed(ordinary);
});

test('themes color interpolation boundaries separately from string text', () => {
  // Generic selectors used by Atom One Dark and VS Code's built-in themes.
  registry.setTheme({ settings: [
    { scope: 'string', settings: { foreground: '#98C379' } },
    { scope: 'string.quoted.double punctuation', settings: { foreground: '#98C379' } },
    { scope: 'punctuation.section.embedded', settings: { foreground: '#BE5046' } },
  ] });
  try {
    for (const line of ['sql"SELECT ${value}, {column}"', '"text {value}"']) {
      const { tokens } = grammar.tokenizeLine2(line, INITIAL);
      const colorAt = (offset) => {
        let metadata;
        for (let i = 0; i < tokens.length && tokens[i] <= offset; i += 2) metadata = tokens[i + 1];
        return registry.getColorMap()[(metadata >>> 15) & 0x1ff];
      };
      assert.equal(colorAt(line.indexOf('"') + 1), '#98C379');
      for (let offset = 0; offset < line.length; offset++) {
        if ('${}'.includes(line[offset])) assert.equal(colorAt(offset), '#BE5046', `${line}: ${offset}`);
      }
    }
  } finally {
    registry.setTheme({ settings: [] });
  }
});

test('SQL multiline templates balance nested code and restore the containing language', () => {
  const result = tokenize([
    'let q = sql"""',
    '    SELECT ${ { /* } */ let value = "nested {42}"; value } },',
    '           ${postgres.param_json_as(payload)?}, {postgres.ident("column")!}',
    '    FROM {table}',
    '    """',
    'let nested = "query={ { let q = sql"SELECT ${1}"; q.params().len() } }"',
    'let done = 1',
  ].join('\n'));
  has(result, 1, 'SELECT', 'string.quoted.triple.sql');
  has(result, 1, '/*', 'comment.block');
  has(result, 1, 'let', 'storage.type');
  has(result, 1, 'nested', 'string.quoted.double');
  has(result, 2, 'param_json_as', 'entity.name.function');
  has(result, 3, 'table', 'meta.interpolation.identifier.sql');
  has(result, 5, 'SELECT', 'string.quoted.double.sql');
  has(result, 5, '1', 'constant.numeric.integer');
  has(result, 6, 'let', 'storage.type');
  closed(result);
});

test('SQL holes reset the string token type and unfinished single-line templates recover', () => {
  const line = 'sql"SELECT ${value}, {column}"';
  const { tokens } = grammar.tokenizeLine2(line, INITIAL);
  const typeAt = (offset) => {
    let metadata;
    for (let i = 0; i < tokens.length && tokens[i] <= offset; i += 2) metadata = tokens[i + 1];
    return (metadata >>> 8) & 3;
  };
  assert.equal(typeAt(line.indexOf('SELECT')), 2);
  assert.equal(typeAt(line.indexOf('value')), 0);
  assert.equal(typeAt(line.indexOf('column')), 0);
  const result = tokenize('sql"unfinished\nlet after = 1');
  has(result, 1, 'let', 'storage.type');
  closed(result);
});

test('raw delimiters preserve quotes, escapes and braces until matching hashes', () => {
  for (const count of [0, 1, 2, 16]) {
    const hashes = '#'.repeat(count);
    const decoy = count ? `"${'#'.repeat(count - 1)}` : '';
    const result = tokenize(`let raw = r${hashes}"\\n {value} ${decoy} tail"${hashes}; let after = 1`);
    has(result, 0, '{value}', 'string.quoted.double.raw');
    has(result, 0, 'tail', 'string.quoted.double.raw');
    has(result, 0, 'after', 'variable.other');
    assert.ok(!scopes(result, 0, '{value}').includes('meta.embedded.inline.joky'));
    closed(result);
  }
});

test('multiline strings preserve state and resume code after their terminators', () => {
  for (const newline of ['\n', '\r\n', '\r']) {
    const result = tokenize(['let text = """', '    hello {name}', '    """', 'let done = 1'].join(newline));
    has(result, 1, 'hello', 'string.quoted.triple');
    has(result, 1, 'name', 'meta.embedded.inline');
    has(result, 3, 'let', 'storage.type');
    closed(result);
  }
});

test('raw multiline strings ignore short terminators and all embedded syntax', () => {
  const result = tokenize('let raw = r##"""\n    """# {name} \\n /*\n    """##; let after = 1');
  has(result, 1, '{name}', 'string.quoted.triple.raw');
  has(result, 1, '/*', 'string.quoted.triple.raw');
  has(result, 2, 'after', 'variable.other');
  closed(result);
});

test('unfinished single-line literals recover on the next line', () => {
  for (const prefix of ['"', 'r##"', "'"]) {
    const result = tokenize(`${prefix}unfinished\nlet after = 1`);
    has(result, 1, 'let', 'storage.type');
    closed(result);
  }
});

test('Rune escapes and quote contents do not open comments or interpolation', () => {
  const result = tokenize(String.raw`'中' '\u{1F642}' '\'' '{' '/' '\q'`);
  has(result, 0, '中', 'string.quoted.single');
  has(result, 0, '\\u{1F642}', 'constant.character.escape');
  has(result, 0, '{', 'string.quoted.single');
  has(result, 0, '\\q', 'invalid.illegal.escape');
  closed(result);
});

test('numeric literals, ranges and tuple indices remain separate', () => {
  const result = tokenize('0xff_ff 0b10_01 1_000 1.25e-2 0..10 1..=4 nested.1.0');
  for (const text of ['0xff_ff', '0b10_01', '1_000', '10']) has(result, 0, text, 'constant.numeric.integer');
  has(result, 0, '1.25e-2', 'constant.numeric.float');
  has(result, 0, '..10', 'keyword.operator');
  has(result, 0, '..=4', 'keyword.operator');
  const tuple = tokenize('nested.1.0');
  has(tuple, 0, '1', 'constant.numeric.integer');
  has(tuple, 0, '0', 'constant.numeric.integer');
  for (const literal of ['1ns', '1us', '1ms', '1s', '2min', '1h', '1.5s', '1_000.000_001ms']) {
    const duration = tokenize(literal);
    has(duration, 0, literal, 'constant.numeric.duration');
    has(duration, 0, literal.match(/[a-z]+$/)[0], 'constant.numeric.duration');
  }
  const boundaries = tokenize('timeout1ms 1ms_more 1m 1e3s');
  assert.ok(boundaries.lines[0].tokens.every((token) => !token.scopes.includes('constant.numeric.duration.joky')));
  const interpolation = tokenize('"delay={1.5s}"');
  has(interpolation, 0, '1.5s', 'constant.numeric.duration');
  closed(interpolation);
});

test('contextual concurrency forms preserve ordinary method calls', () => {
  const result = tokenize('join { first(), second() }\ntext.join(values)\nisolate { work() }\nwhen "update" (mut state) {}\naliased {}\naliased()');
  has(result, 0, 'join', 'keyword.control');
  has(result, 1, 'join', 'entity.name.function');
  has(result, 2, 'isolate', 'keyword.control');
  has(result, 3, 'when', 'keyword.control');
  has(result, 4, 'aliased', 'keyword.control');
  has(result, 5, 'aliased', 'entity.name.function');
  closed(result);
});

test('language configuration supports comments, words and basic indentation', () => {
  assert.deepEqual(configuration.comments, { lineComment: '//', blockComment: ['/*', '*/'] });
  const word = new RegExp(configuration.wordPattern, 'g');
  assert.deepEqual('user.名字 next_item'.match(word), ['user', '名字', 'next_item']);
  const increase = new RegExp(configuration.indentationRules.increaseIndentPattern);
  const decrease = new RegExp(configuration.indentationRules.decreaseIndentPattern);
  assert.ok(increase.test('fn main() { // body'));
  assert.ok(increase.test('    let items = ['));
  assert.ok(!increase.test('// fn main() {'));
  assert.ok(!increase.test('fn empty() {}'));
  assert.ok(decrease.test('    } else {'));
  assert.ok(decrease.test('    ]'));
});

// Real programs catch long-lived scopes that a small synthetic case can miss.
test('all examples and standard modules return to the root scope', () => {
  for (const directory of ['examples', 'src/stdlib']) {
    const base = path.join(root, '../..', directory);
    for (const entry of readdirSync(base, { recursive: true })) {
      if (!entry.endsWith('.jk')) continue;
      const source = readFileSync(path.join(base, entry), 'utf8');
      const result = tokenize(`${source}\nlet highlighting_sentinel = 1`);
      has(result, result.lines.length - 1, 'let', 'storage.type');
      assert.ok(result.state.equals(grammar.tokenizeLine('', INITIAL).ruleStack), `${directory}/${entry}`);
    }
  }
});
