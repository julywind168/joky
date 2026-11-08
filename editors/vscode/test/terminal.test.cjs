const assert = require('node:assert/strict');
const Module = require('node:module');
const { test } = require('node:test');

const file = require.resolve('../src/terminal.cjs');
const load = Module._load;
Module._load = function (request, parent, ...rest) {
  if (parent?.filename === file && request === 'vscode') return {
    EventEmitter: class {
      listeners = new Set();
      event = callback => { this.listeners.add(callback); return { dispose: () => this.listeners.delete(callback) }; };
      fire(value) { for (const callback of this.listeners) callback(value); }
      dispose() { this.listeners.clear(); }
    },
  };
  return load.call(this, request, parent, ...rest);
};
let processTerminal;
try { ({ processTerminal } = require(file)); } finally { Module._load = load; }

test('single-file terminals accept line input and retain completed output until closed', async t => {
  const terminal = processTerminal(process.execPath, ['-e', 'process.stdin.pipe(process.stdout)'], process.cwd());
  t.after(() => terminal.close());
  let output = '';
  const completed = new Promise((resolve, reject) => {
    const timer = setTimeout(() => reject(new Error('terminal did not finish')), 5000);
    t.after(() => clearTimeout(timer));
    terminal.onDidWrite(data => {
      output += data;
      if (output.includes('[Joky exited with')) resolve();
    });
  });
  terminal.open();
  terminal.handleInput('hello🙂\x7f!\r');
  terminal.handleInput('\x04');
  await completed;
  assert.ok(output.includes('hello!\r\n'), output);
  assert.ok(output.includes('[Joky exited with code 0]'), output);
  assert.equal(terminal.onDidClose, undefined, 'completed output stays visible');
});
