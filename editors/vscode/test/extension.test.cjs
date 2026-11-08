const assert = require('node:assert/strict');
const Module = require('node:module');
const { test } = require('node:test');
const toolchain = require('../src/toolchain.cjs');

function harness(overrides = {}, trusted = true) {
  let provider;
  const messages = [];
  const commands = new Map();
  const disposable = { dispose() {} };
  const vscode = {
    window: {
      createOutputChannel: () => ({ appendLine() {}, show() {}, dispose() {} }),
      showErrorMessage: async message => { messages.push(message); },
      showInformationMessage: async message => { messages.push(message); },
    },
    workspace: {
      isTrusted: trusted,
      getConfiguration: () => ({ get: () => 'joky' }),
      onDidChangeConfiguration: () => disposable,
    },
    commands: { registerCommand: (name, callback) => { commands.set(name, callback); return disposable; } },
    languages: { registerDocumentFormattingEditProvider: (_, value) => { provider = value; return disposable; } },
    Range: class { constructor(start, end) { this.start = start; this.end = end; } },
    TextEdit: { replace: (range, text) => ({ range, text }) },
  };
  const tools = {
    ...toolchain, resolveExecutable: async () => '/compiler/joky',
    execute: async (_, args) => args[0] === '--version' ? 'joky 0.1.0' : 'formatted', ...overrides,
  };
  const file = require.resolve('../src/extension.cjs');
  delete require.cache[file];
  const load = Module._load;
  Module._load = function (request, parent, ...rest) {
    if (parent?.filename === file && request === 'vscode') return vscode;
    if (parent?.filename === file && request === './toolchain.cjs') return tools;
    if (parent?.filename === file && request === './terminal.cjs') return { processTerminal() {} };
    return load.call(this, request, parent, ...rest);
  };
  const subscriptions = [];
  try { require(file).activate({ subscriptions }); } finally { Module._load = load; }
  const document = {
    uri: { scheme: 'untitled' }, version: 1, isClosed: false,
    getText: () => 'original', positionAt: offset => offset,
  };
  let cancel = () => {};
  const token = { isCancellationRequested: false, onCancellationRequested(callback) { cancel = callback; return disposable; } };
  return {
    format: () => provider.provideDocumentFormattingEdits(document, {}, token), document, messages, commands,
    cancel: () => { token.isCancellationRequested = true; cancel(); },
    dispose: () => subscriptions.forEach(value => value.dispose()),
  };
}

test('untrusted workspaces never launch a compiler or program', async () => {
  const h = harness({ resolveExecutable: () => { throw new Error('must not launch'); } }, false);
  assert.deepEqual(await h.format(), []);
  await h.commands.get('joky.run')();
  assert.equal(h.messages.length, 2);
  h.dispose();
});

test('a missing compiler leaves formatting untouched and only prompts once', async () => {
  const h = harness({ resolveExecutable: async () => { throw Object.assign(new Error('missing'), { code: 'ENOENT' }); } });
  assert.deepEqual(await h.format(), []);
  assert.deepEqual(await h.format(), []);
  assert.equal(h.messages.length, 1);
  h.dispose();
});

test('format errors preserve the buffer without waiting for notification dismissal', async () => {
  const h = harness({ execute: async (_, args) => {
    if (args[0] === '--version') return 'joky 0.1.0';
    throw new Error('Syntax error');
  } });
  assert.deepEqual(await h.format(), []);
  assert.equal(h.messages.length, 1);
  h.dispose();
});

test('stale or cancelled formatter results never produce edits', async () => {
  for (const action of ['edit', 'cancel', 'dispose']) {
    let finish;
    let started;
    let signal;
    const ready = new Promise(resolve => { started = resolve; });
    const h = harness({ execute: async (_, args, options) => {
      if (args[0] === '--version') return 'joky 0.1.0';
      signal = options.signal;
      started();
      return new Promise(resolve => { finish = resolve; });
    } });
    const result = h.format();
    await ready;
    if (action === 'edit') h.document.version++;
    else if (action === 'cancel') h.cancel();
    else h.dispose();
    finish('formatted');
    assert.deepEqual(await result, []);
    if (action !== 'edit') assert.equal(signal.aborted, true);
    h.dispose();
  }
});
