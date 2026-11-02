/* Disposable debug emulator only. Synthetic origins, local CDP HTML; no login or model calls. */
const { chromium } = require('playwright');
const { execFileSync } = require('node:child_process');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const serial = process.env.ANDROID_SERIAL || 'emulator-5584';
const defaultOrigin = process.env.RPI_WEB_ANDROID_DEFAULT_ORIGIN;
assert.match(serial, /^emulator-\d+$/);
assert.ok(defaultOrigin && new URL(defaultOrigin).hostname.endsWith('.example') && !new URL(defaultOrigin).port,
  'Use a portless reserved example build origin, never a live node.');
assert.ok(process.argv.includes('--reset'), 'Pass --reset explicitly: this clears only the disposable debug test app.');
const app = 'cn.bigfisharc.rpiweb.local';
const customOrigin = 'https://custom.example';
const out = path.resolve(__dirname, '../artifacts');
fs.mkdirSync(out, { recursive: true });
const adb = (...args) => execFileSync('adb', ['-s', serial, ...args], { encoding: 'utf8', timeout: 30000 });
const sleep = ms => new Promise(r => setTimeout(r, ms));
let browser, port, page;
const checks = [];
function ok(name) { checks.push(name); console.log('PASS', name); }
function dump() {
  adb('shell', 'uiautomator', 'dump', '/sdcard/rpi-saved-sites-ui.xml');
  return adb('shell', 'cat', '/sdcard/rpi-saved-sites-ui.xml');
}
async function waitText(text) {
  for (let i = 0; i < 15; i++) {
    const xml = dump();
    const node = [...xml.matchAll(/<node\b[^>]*>/g)].map(m => m[0]).find(n =>
      n.includes(`text="${text}"`) || n.includes(`content-desc="${text}"`));
    if (node) return node;
    await sleep(200);
  }
  throw new Error('Expected native UI state missing (values redacted)');
}
async function tap(text) {
  const node = await waitText(text);
  const [, x1, y1, x2, y2] = node.match(/bounds="\[(\d+),(\d+)\]\[(\d+),(\d+)\]"/);
  adb('shell', 'input', 'tap', String((+x1 + +x2) >> 1), String((+y1 + +y2) >> 1));
  await sleep(250);
}
function shot(name) {
  fs.writeFileSync(path.join(out, `saved-sites-${name}.png`), execFileSync('adb', ['-s', serial, 'exec-out', 'screencap', '-p'], { timeout: 30000 }));
}
async function disconnect() {
  if (browser) { await browser.close().catch(() => {}); browser = undefined; }
  if (port) { adb('forward', '--remove', `tcp:${port}`); port = undefined; }
}
async function start() {
  await disconnect();
  adb('shell', 'am', 'force-stop', app);
  adb('shell', 'am', 'start', '-W', '-n', `${app}/.MainActivity`);
  await waitText('站点设置');
}
async function fixture(origin) {
  await disconnect();
  const pid = adb('shell', 'pidof', app).trim().split(' ')[0];
  port = adb('forward', 'tcp:0', `localabstract:webview_devtools_remote_${pid}`).trim();
  browser = await chromium.connectOverCDP(`http://127.0.0.1:${port}`, { noDefaults: true, timeout: 20000 });
  page = browser.contexts()[0].pages()[0];
  assert.ok(page, 'Debug WebView target required');
  await page.route('**/*', route => route.fulfill({ contentType: 'text/html', body:
    '<!doctype html><meta name="viewport" content="width=device-width, initial-scale=1"><h1>Local saved-site fixture</h1><textarea aria-label="Draft"></textarea>' }));
  await page.goto(`${origin}/__saved_sites_fixture__`);
  await page.locator('textarea').fill('keep this unsent draft');
  await page.evaluate(() => { window.fixtureMarker = 'retained'; });
}
async function retained() {
  assert.ok(await page.evaluate(() => window.fixtureMarker === 'retained' && document.querySelector('textarea')?.value === 'keep this unsent draft'), 'Saving/removing must not reload the page or discard its draft');
}
const saved = origin => `已保存：${origin}`;
(async () => {
  adb('shell', 'run-as', app, 'id'); // Refuse non-debuggable release installations.
  adb('shell', 'pm', 'clear', app);
  adb('shell', 'cmd', 'uimode', 'night', 'no');
  await start();
  await fixture(defaultOrigin);
  await tap('站点设置'); await tap('自定义 HTTPS 站点');
  adb('shell', 'run-as', app, 'mkdir', '-p', 'shared_prefs');
  const mode = adb('shell', 'run-as', app, 'stat', '-c', '%a', 'shared_prefs').trim();
  assert.match(mode, /^[0-7]{3,4}$/);
  try {
    adb('shell', 'run-as', app, 'chmod', '500', 'shared_prefs');
    await tap('保存');
    await waitText('保存'); await retained();
    shot('storage-failure');
  } finally {
    adb('shell', 'run-as', app, 'chmod', mode, 'shared_prefs');
  }
  await tap('保存');
  await waitText(saved(defaultOrigin));
  ok('Failed disk write keeps the editor open and restoring storage allows a successful retry');
  await retained();
  shot('saved-list-day');
  ok('Save adds the prefilled origin to the local list without switching or losing the draft');

  await tap('自定义 HTTPS 站点'); await tap('HTTPS 站点地址'); await tap('保存');
  // UIAutomator omits the separate TextView error popup. Check editor/storage
  // state automatically and keep local screenshots for visual feedback review.
  await waitText('保存');
  const countSaved = () => {
    const xml = adb('shell', 'run-as', app, 'cat', 'shared_prefs/connection.xml');
    return (xml.match(/<set name="saved_origins">([\s\S]*?)<\/set>/)?.[1].match(/<string>/g) || []).length;
  };
  assert.equal(countSaved(), 1);
  shot('duplicate-feedback');
  ok('Duplicate save stays in the editor and leaves exactly one stored bookmark');
  await tap('HTTPS 站点地址');
  adb('shell', 'input', 'keycombination', '113', '29');
  adb('shell', 'input', 'keyevent', '67');
  await tap('保存');
  assert.ok((await waitText('HTTPS 站点地址')).includes('text=""'), 'Invalid empty entry stays in the editor');
  assert.equal(countSaved(), 1);
  shot('invalid-feedback');
  await retained();
  ok('Invalid empty origin is rejected without switching or altering the saved list');
  adb('shell', 'input', 'keyevent', '4'); // Dismiss the IME/error popup first.
  await tap('取消');

  await start(); await fixture(defaultOrigin);
  await tap('站点设置'); await waitText(saved(defaultOrigin));
  await tap('自定义 HTTPS 站点'); await tap('HTTPS 站点地址');
  // Edit only the host stem, retaining URI punctuation. Some emulator IME locales
  // transform punctuation supplied by adb input text into full-width characters.
  adb('shell', 'input', 'keyevent', '123');
  adb('shell', 'input', 'keyevent', ...Array(8).fill('21')); // Before .example
  adb('shell', 'input', 'keyevent', ...Array(new URL(defaultOrigin).hostname.length - 8).fill('67'));
  adb('shell', 'input', 'text', 'custom');
  assert.ok((await waitText('HTTPS 站点地址')).includes(`text="${customOrigin}"`), 'Native edited origin must match fixture');
  await tap('保存');
  await waitText(saved(customOrigin)); await waitText(saved(defaultOrigin));
  await retained();
  ok('Saved entries survive cold restart and a natively edited custom origin can be added without switching');
  await tap(saved(customOrigin)); await tap('切换'); await waitText(`当前站点：${customOrigin}`);
  await fixture(customOrigin);

  await tap('站点设置'); await tap(saved(defaultOrigin)); await waitText('切换连接站点？'); await tap('取消');
  await retained(); await waitText(`当前站点：${customOrigin}`);
  ok('Selecting a saved origin still requires switch confirmation; Cancel preserves the page');
  await tap('站点设置'); await tap(saved(defaultOrigin)); await tap('切换');
  await waitText(`当前站点：${defaultOrigin}`);
  await start(); await waitText(`当前站点：${defaultOrigin}`); await fixture(defaultOrigin);
  ok('Confirmed saved-origin selection persists as the active connection across restart');

  await tap('站点设置'); await tap(saved(defaultOrigin)); await waitText('当前连接站点');
  const pid = adb('shell', 'pidof', app).trim();
  adb('shell', 'cmd', 'uimode', 'night', 'yes'); await sleep(1000);
  await waitText('移除保存'); await retained(); assert.equal(adb('shell', 'pidof', app).trim(), pid);
  shot('current-dialog-night');
  adb('shell', 'cmd', 'uimode', 'night', 'no'); await sleep(500);
  await waitText('移除保存'); await retained(); shot('current-dialog-day');
  ok('Saved-origin dialog follows live OS day/night without Activity or WebView recreation');

  await tap('移除保存'); await waitText('移除已保存站点？'); await tap('取消');
  await tap('站点设置'); await waitText(saved(defaultOrigin));
  ok('Removing a bookmark requires a separate confirmation and Cancel keeps it');
  await tap(saved(defaultOrigin)); await tap('移除保存'); await tap('移除');
  assert.ok(!dump().includes(saved(defaultOrigin)), 'Removed bookmark must disappear');
  await waitText(saved(customOrigin)); await tap('取消'); await waitText(`当前站点：${defaultOrigin}`); await retained();
  ok('Removing the active bookmark does not disconnect or reload its page');
  await tap('站点设置'); await tap(saved(customOrigin)); await tap('移除保存'); await tap('移除');
  assert.ok(!dump().includes('已保存：'), 'Both fixture bookmarks should be removed');
  await tap('取消'); await start(); await tap('站点设置');
  assert.ok(!dump().includes('已保存：'), 'Removals must persist across cold restart');
  await tap('取消'); await waitText(`当前站点：${defaultOrigin}`);
  ok('Removing a different bookmark and restarting preserves the active connection and empty list');
  fs.writeFileSync(path.join(out, 'saved-sites-report.json'), JSON.stringify({ status: 'passed', serial, checks, realModelCalls: 0, authentication: 'not attempted', fixture: 'reserved example origins, native URL editing, debug run-as storage fault, CDP-intercepted HTML only' }, null, 2));
})().catch(() => {
  console.error('Saved-site smoke failed; inspect ignored local artifacts (not for publication).');
  try { fs.writeFileSync(path.join(out, 'saved-sites-failure.xml'), dump()); shot('failure'); } catch {}
  process.exitCode = 1;
}).finally(async () => {
  await disconnect().catch(() => {});
  try { adb('shell', 'cmd', 'uimode', 'night', 'no'); } catch {}
});
