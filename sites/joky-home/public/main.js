const samples = {
  hello: { filename: 'hello.jk', output: 'Hello, world!', code: 'fn main() {\n    println("Hello, world!")\n}' },
  loop: { filename: 'loops.jk', output: '45', code: 'fn sum(n: Int32) -> Int32 {\n    var total = 0\n    for value in 0..n { total += value }\n    total\n}\n\nfn main() {\n    println(sum(10))\n}' }
};
let selected = 'hello';
const tabs = [...document.querySelectorAll('[data-sample]')];
const codeElement = document.getElementById('sample-code');
function renderSample(name) {
  selected = name;
  const sample = samples[name];
  codeElement.replaceChildren();
  const tokens = sample.code.split(/("(?:\\.|[^"\\])*"|\b(?:fn|var|let|for|while|import)\b|\b\d+\b|\b(?:println|main|sum)\b)/g);
  tokens.forEach(token => {
    const span = document.createElement('span');
    span.textContent = token;
    if (/^"/.test(token)) span.className = 'string';
    else if (/^(fn|var|let|for|while|import)$/.test(token)) span.className = 'keyword';
    else if (/^\d+$/.test(token)) span.className = 'number';
    else if (/^(println|main|sum)$/.test(token)) span.className = 'function';
    codeElement.append(span);
  });
  document.getElementById('sample-filename').textContent = sample.filename;
  document.getElementById('sample-output').textContent = sample.output;
  tabs.forEach(tab => {
    const active = tab.dataset.sample === name;
    tab.setAttribute('aria-selected', String(active));
    tab.tabIndex = active ? 0 : -1;
    if (active) document.getElementById('sample-panel').setAttribute('aria-labelledby', tab.id);
  });
}
tabs.forEach((tab, index) => {
  tab.addEventListener('click', () => renderSample(tab.dataset.sample));
  tab.addEventListener('keydown', event => {
    let next;
    if (event.key === 'ArrowRight') next = (index + 1) % tabs.length;
    if (event.key === 'ArrowLeft') next = (index - 1 + tabs.length) % tabs.length;
    if (event.key === 'Home') next = 0;
    if (event.key === 'End') next = tabs.length - 1;
    if (next === undefined) return;
    event.preventDefault();
    renderSample(tabs[next].dataset.sample);
    tabs[next].focus();
  });
});
let toastTimer;
function showToast(message) {
  const toast = document.getElementById('toast');
  toast.textContent = message;
  toast.classList.add('visible');
  clearTimeout(toastTimer);
  toastTimer = setTimeout(() => toast.classList.remove('visible'), 2800);
}
async function copyText(value, successMessage) {
  try {
    await navigator.clipboard.writeText(value);
    showToast(successMessage);
  } catch {
    showToast('复制未成功，请选中文本后手动复制。');
  }
}
document.getElementById('copy-code').addEventListener('click', () => copyText(samples[selected].code, '代码已复制，带上它开始探索吧。'));
document.getElementById('copy-command').addEventListener('click', () => copyText('cargo run -- run examples/hello.jk', '运行命令已复制。'));
renderSample(selected);
