const assert = require('node:assert/strict');
const fs = require('node:fs/promises');
const os = require('node:os');
const path = require('node:path');
const { test } = require('node:test');
const { resolveExecutable, resolveTarget, contains, execute, replacement } = require('../src/toolchain.cjs');

async function scratch(t) {
  const root = await fs.mkdtemp(path.join(os.tmpdir(), 'joky editor '));
  t.after(() => fs.rm(root, { recursive: true, force: true }));
  return root;
}

test('compiler discovery accepts PATH and absolute paths without interpreting shell text', async t => {
  const root = await scratch(t);
  const executable = path.join(root, 'joky ; $test');
  await fs.writeFile(executable, 'test', { mode: 0o755 });
  assert.equal(await resolveExecutable(executable), executable);
  assert.equal(await resolveExecutable(path.basename(process.execPath), { PATH: path.dirname(process.execPath) }), process.execPath);
  await assert.rejects(resolveExecutable('./joky'), /absolute path/);
  await assert.rejects(resolveExecutable('missing-joky', { PATH: root }), { code: 'ENOENT' });
  assert.equal(await execute(process.execPath, ['-e', 'process.stdout.write(process.argv[1])', 'a ; $(echo bad)']), 'a ; $(echo bad)');
});

test('target selection uses the nearest manifest within the selected workspace', async t => {
  const root = await scratch(t);
  const nested = path.join(root, 'nested');
  await fs.mkdir(path.join(nested, 'src'), { recursive: true });
  const file = path.join(nested, 'src', 'hello.jk');
  await fs.writeFile(file, '');
  await fs.writeFile(path.join(root, 'joky.toml'), '');
  assert.deepEqual(await resolveTarget(file, root), { target: path.join(root, 'joky.toml'), cwd: root, project: true });
  assert.deepEqual(await resolveTarget(file, nested), { target: file, cwd: path.dirname(file), project: false });
  await fs.writeFile(path.join(nested, 'joky.toml'), '');
  assert.deepEqual(await resolveTarget(file, root), { target: path.join(nested, 'joky.toml'), cwd: nested, project: true });
  assert.equal((await resolveTarget(nested, root)).cwd, nested);
  assert.equal(contains(root, `${root}-other/source.jk`), false);
});

test('process input, failures, cancellation and timeout are bounded', async () => {
  assert.equal(await execute(process.execPath, ['-e', 'process.stdin.pipe(process.stdout)'], { input: '中文\r\n🙂' }), '中文\r\n🙂');
  await assert.rejects(execute(process.execPath, ['-e', 'process.stderr.write("syntax error"); process.exit(1)']), /syntax error/);
  const controller = new AbortController();
  const pending = execute(process.execPath, ['-e', 'setInterval(() => {}, 1000)'], { signal: controller.signal });
  controller.abort();
  await assert.rejects(pending, { name: 'AbortError' });
  await assert.rejects(execute(process.execPath, ['-e', 'setInterval(() => {}, 1000)'], { timeout: 100 }));
});

test('format replacements preserve unchanged text and never split UTF-16 pairs or CRLF', () => {
  assert.equal(replacement('same', 'same'), undefined);
  for (const [before, after] of [
    ['hello x\nworld', 'hello y\nworld'], ['🙂x', '🙃x'], ['a\r\nb', 'a\nb'],
    ['a\rb', 'a\r\nb'], ['', 'text'], ['text', ''], ['中文\r\nx', '中文\r\ny'],
  ]) {
    const edit = replacement(before, after);
    assert.equal(before.slice(0, edit.start) + edit.text + before.slice(edit.end), after);
    for (const at of [edit.start, edit.end]) {
      assert.ok(!(before[at - 1] === '\r' && before[at] === '\n'));
      assert.ok(!(/[\uD800-\uDBFF]/.test(before[at - 1] || '') && /[\uDC00-\uDFFF]/.test(before[at] || '')));
    }
  }
});
