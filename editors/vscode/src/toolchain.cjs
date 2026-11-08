const fs = require('node:fs/promises');
const { constants } = require('node:fs');
const path = require('node:path');
const { execFile } = require('node:child_process');

async function isFile(file) {
  try { return (await fs.stat(file)).isFile(); } catch { return false; }
}

async function resolveExecutable(setting, env = process.env) {
  const name = setting.trim();
  if (!name) throw new Error('Set joky.executablePath to an executable name or absolute path.');
  if (!path.isAbsolute(name) && /[/\\]/.test(name)) {
    throw new Error('joky.executablePath must be an absolute path or an executable name on PATH.');
  }
  const names = process.platform === 'win32' && !path.extname(name) ? [`${name}.exe`] : [name];
  const candidates = path.isAbsolute(name) ? names : (env.PATH || env.Path || '')
    .split(path.delimiter).filter(path.isAbsolute)
    .flatMap(directory => names.map(file => path.join(directory, file)));
  for (const candidate of candidates) {
    if (!await isFile(candidate)) continue;
    try {
      await fs.access(candidate, constants.X_OK);
      return candidate;
    } catch { /* Try the next PATH entry. */ }
  }
  const error = new Error(`Cannot find the Joky compiler: ${name}`);
  error.code = 'ENOENT';
  throw error;
}

function contains(directory, file) {
  const relative = path.relative(directory, file);
  return relative === '' || (!relative.startsWith(`..${path.sep}`) && relative !== '..' && !path.isAbsolute(relative));
}

async function resolveTarget(file, workspaceRoot) {
  const directory = (await fs.stat(file)).isDirectory();
  let current = directory ? file : path.dirname(file);
  const boundary = workspaceRoot && contains(workspaceRoot, current) ? workspaceRoot : path.parse(current).root;
  while (true) {
    const manifest = path.join(current, 'joky.toml');
    if (await isFile(manifest)) return { target: manifest, cwd: current, project: true };
    if (current === boundary || current === path.dirname(current)) break;
    current = path.dirname(current);
  }
  if (directory || path.extname(file) !== '.jk') {
    throw new Error('Open a .jk file or a folder containing joky.toml.');
  }
  return { target: file, cwd: path.dirname(file), project: false };
}

function execute(executable, args, { cwd, input = '', signal, timeout = 15000 } = {}) {
  return new Promise((resolve, reject) => {
    const child = execFile(executable, args, {
      cwd, signal, timeout, maxBuffer: 16 * 1024 * 1024, encoding: 'utf8', windowsHide: true,
      env: { ...process.env, NO_COLOR: '1', CLICOLOR_FORCE: '0' },
    }, (error, stdout, stderr) => {
      if (error) {
        error.message = stderr.trim() || error.message;
        reject(error);
      } else resolve(stdout);
    });
    // A compiler that rejects its arguments may close stdin before all input is written.
    child.stdin.on('error', () => {});
    child.stdin.end(input);
  });
}

function replacement(before, after) {
  if (before === after) return undefined;
  let start = 0;
  while (start < before.length && start < after.length && before[start] === after[start]) start++;
  const splitPair = (text, at) => at > 0 && at < text.length && (
    (text[at - 1] === '\r' && text[at] === '\n') ||
    (/[\uD800-\uDBFF]/.test(text[at - 1]) && /[\uDC00-\uDFFF]/.test(text[at]))
  );
  if (splitPair(before, start) || splitPair(after, start)) start--;
  let end = before.length;
  let newEnd = after.length;
  while (end > start && newEnd > start && before[end - 1] === after[newEnd - 1]) { end--; newEnd--; }
  if (splitPair(before, end) || splitPair(after, newEnd)) { end++; newEnd++; }
  return { start, end, text: after.slice(start, newEnd) };
}

module.exports = { resolveExecutable, resolveTarget, contains, execute, replacement };
