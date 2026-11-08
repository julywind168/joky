const fs = require('node:fs/promises');
const assert = require('node:assert/strict');
const os = require('node:os');
const path = require('node:path');
const { spawn } = require('node:child_process');

async function main() {
  const root = await fs.mkdtemp(path.join(os.tmpdir(), 'joky-editor-host-'));
  const workspace = path.join(root, 'workspace');
  await fs.mkdir(workspace);
  const extension = path.resolve(__dirname, '..');
  const compiler = process.env.JOKY_TEST_COMPILER || path.resolve(extension, '../../target/debug/joky');
  await fs.access(compiler);
  try {
    const child = spawn(process.env.VSCODE_CLI || 'code', [
      '--extensionDevelopmentPath', extension,
      '--extensionTestsPath', path.join(__dirname, 'host/index.cjs'),
      '--user-data-dir', path.join(root, 'profile'), '--extensions-dir', path.join(root, 'extensions'),
      '--disable-workspace-trust', '--skip-welcome', '--skip-release-notes', '--disable-extensions', '--wait',
      ...(process.env.JOKY_TEST_NO_FOLDER ? [] : [workspace]),
    ], { stdio: 'inherit', env: { ...process.env, JOKY_TEST_COMPILER: compiler, JOKY_TEST_WORKSPACE: workspace } });
    const code = await new Promise((resolve, reject) => {
      child.on('error', reject);
      child.on('exit', resolve);
    });
    if (code !== 0) throw new Error(`Extension host exited with ${code}`);
    assert.equal(await fs.readFile(path.join(workspace, 'host-result.txt'), 'utf8'), 'passed');
    console.log('Joky extension host checks passed.');
  } finally { await fs.rm(root, { recursive: true, force: true }); }
}

main().catch(error => { console.error(error); process.exitCode = 1; });
