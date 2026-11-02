/* Debug emulator only: real adb touches, reserved fixture links, no login or model calls. */
const { chromium } = require('playwright');
const { execFileSync } = require('node:child_process');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const serial = process.env.ANDROID_SERIAL || 'emulator-5584';
const origin = process.env.RPI_WEB_ANDROID_DEFAULT_ORIGIN;
assert.match(serial, /^emulator-\d+$/);
assert.ok(origin && new URL(origin).hostname.endsWith('.example'), 'Use a reserved example build origin');
assert.ok(process.argv.includes('--reset'), 'Pass --reset explicitly for this disposable debug app');
const app = 'cn.bigfisharc.rpiweb.local';
const http = 'http://192.0.2.7:13031'; // Documentation IP, NOT an operator node.
const out = path.resolve(__dirname, '../artifacts');
fs.mkdirSync(out, { recursive: true });
const adb = (...args) => execFileSync('adb', ['-s', serial, ...args], { encoding: 'utf8', timeout: 30000 });
const sleep = ms => new Promise(r => setTimeout(r, ms));
const html = `<!doctype html><meta name="viewport" content="width=device-width,initial-scale=1">
<style>body{font:16px sans-serif;padding:12px}a,button{display:block;min-height:44px;margin:8px 0}</style>
<a id="https" href="https://target.example/article">HTTPS regression</a>
<a id="plain" href="${http}/downloads/client.apk">Plain HTTP download</a>
<a id="markdown" href="${http}/downloads/client.apk" target="_blank" rel="noopener noreferrer">Markdown HTTP APK download</a>
<a id="nested" href="${http}/downloads/client.apk" target="_blank" rel="noopener noreferrer"><strong>Nested HTTP link</strong></a>
<a id="script" href="https://decoy.example" onclick="event.preventDefault();window.open('${http}/actual?fixture=1','_blank')">Actual popup target differs from href</a>
<button id="noopener" onclick="window.open('${http}/actual','_blank','noopener,noreferrer')">Noopener HTTP popup</button>
<a id="unsafe" href="intent://scan/#Intent;scheme=fixture;end">Unsupported scheme</a>
<a id="credentials" href="http://user:pass@192.0.2.7:13031/client.apk" target="_blank">Rejected embedded credentials</a>
<textarea aria-label="Draft"></textarea>
<script>window.clicks=[];document.addEventListener('click',e=>clicks.push({id:e.target.closest('a,button')?.id,trusted:e.isTrusted}),true);</script>`;
let browser, port, page;
const checks = [], httpRequests = [];
function ok(name) { checks.push(name); console.log('PASS', name); }
function dump() {
  adb('shell', 'uiautomator', 'dump', '/sdcard/rpi-links-ui.xml');
  return adb('shell', 'cat', '/sdcard/rpi-links-ui.xml');
}
function node(xml, text) {
  return [...xml.matchAll(/<node\b[^>]*>/g)].map(m => m[0]).find(n => n.includes(`text="${text}"`) || n.includes(`content-desc="${text}"`));
}
async function tapNative(text) {
  const n = node(dump(), text);
  assert.ok(n, 'Expected native action missing');
  const [, x1, y1, x2, y2] = n.match(/bounds="\[(\d+),(\d+)\]\[(\d+),(\d+)\]"/);
  adb('shell', 'input', 'tap', String((+x1 + +x2) >> 1), String((+y1 + +y2) >> 1));
  await sleep(250);
}
async function fixture() {
  await page.goto(`${origin}/__link_test__`);
  // Seed the local draft without focusing the IME. Sending Back during an IME
  // hide race can navigate history instead and invalidate this fixture.
  await page.evaluate(() => {
    window.fixtureMarker = 'retained';
    document.querySelector('textarea').value = 'keep this unsent draft';
  });
}
async function retained() {
  assert.ok(await page.evaluate(() => window.fixtureMarker === 'retained' && document.querySelector('textarea')?.value === 'keep this unsent draft'), 'External handoff must preserve the original document and draft');
  assert.ok(page.url().startsWith(origin + '/'), 'External HTTP must never become the trusted page');
}
async function touch(id) {
  const link = page.locator('#' + id);
  await link.scrollIntoViewIfNeeded();
  const xml = dump();
  const web = [...xml.matchAll(/<node\b[^>]*>/g)].map(m => m[0]).find(n => n.includes('class="android.webkit.WebView"'));
  assert.ok(web, 'Native WebView bounds required');
  const [, wx1, wy1, wx2] = web.match(/bounds="\[(\d+),(\d+)\]\[(\d+),(\d+)\]"/);
  const box = await link.boundingBox();
  const ratio = (+wx2 - +wx1) / await page.evaluate(() => innerWidth);
  adb('shell', 'input', 'tap', String(Math.round(+wx1 + (box.x + Math.min(box.width / 2, 100)) * ratio)),
    String(Math.round(+wy1 + (box.y + box.height / 2) * ratio)));
  await sleep(350);
  assert.ok(await page.evaluate(id => clicks.some(c => c.id === id && c.trusted), id), 'A real trusted DOM click must be observed');
}
async function confirmation(id, insecure) {
  await fixture(); await touch(id);
  let xml = '';
  for (let n = 0; n < 5; n++) { xml = dump(); if (node(xml, '在系统浏览器中打开？')) break; await sleep(200); }
  assert.ok(node(xml, '在系统浏览器中打开？'), 'Expected browser confirmation missing');
  assert.equal(xml.includes('HTTP 链接未加密'), insecure, 'HTTP warning must match the actual destination scheme');
  if (insecure) { assert.ok(xml.includes(http)); assert.ok(!xml.includes('decoy.example')); }
  await retained();
  return xml;
}
function shot(name) { fs.writeFileSync(path.join(out, `links-${name}.png`), execFileSync('adb', ['-s', serial, 'exec-out', 'screencap', '-p'], { timeout: 30000 })); }
(async () => {
  adb('shell', 'run-as', app, 'id');
  adb('shell', 'pm', 'clear', app);
  adb('shell', 'am', 'start', '-W', '-n', `${app}/.MainActivity`);
  await sleep(2000);
  const pid = adb('shell', 'pidof', app).trim().split(' ')[0];
  port = adb('forward', 'tcp:0', `localabstract:webview_devtools_remote_${pid}`).trim();
  browser = await chromium.connectOverCDP(`http://127.0.0.1:${port}`, { noDefaults: true, timeout: 20000 });
  page = browser.contexts()[0].pages()[0];
  page.on('request', request => { if (request.url().startsWith('http://')) httpRequests.push(request.url()); });
  await page.route('**/*', route => route.fulfill({ contentType: 'text/html', body: html }));
  // CDP-fulfilled attachment responses do not exercise Android's real network
  // DownloadListener. That callback is verified separately with a static file.
  for (const [id, insecure] of [['https', false], ['plain', true], ['markdown', true], ['nested', true], ['script', true], ['noopener', true]]) {
    await confirmation(id, insecure);
    if (id === 'markdown') shot('http-download-warning');
    await tapNative('取消'); await retained();
    ok(`${id}: real touch shows correct confirmation; Cancel preserves page/draft`);
  }
  for (const id of ['unsafe', 'credentials']) {
    await fixture(); await touch(id);
    assert.ok(!node(dump(), '在系统浏览器中打开？'), 'Unsupported URL must not be offered to a browser');
    await retained();
    const state = adb('shell', 'dumpsys', 'activity', 'activities');
    assert.ok(state.split('\n').some(line => /mResumedActivity|topResumedActivity/.test(line) && line.includes(app)), 'Unsafe target must not leave the app');
    ok(`${id}: no browser handoff or trusted-page navigation`);
  }
  assert.equal(httpRequests.length, 0, 'HTTP must not be fetched by the authenticated WebView');
  ok('No fixture HTTP requests entered the authenticated main WebView');
  await confirmation('markdown', true); await tapNative('打开'); await sleep(1000);
  const state = adb('shell', 'dumpsys', 'activity', 'activities');
  fs.writeFileSync(path.join(out, 'links-browser-intent.txt'), state);
  assert.ok(state.includes('android.intent.action.VIEW') && state.includes('192.0.2.7:13031'), 'Browser ACTION_VIEW must retain the HTTP host and explicit port');
  adb('shell', 'am', 'start', '-W', '-f', '0x00020000', '-n', `${app}/.MainActivity`);
  await sleep(700); await retained();
  ok('Explicit Open dispatches HTTP ACTION_VIEW and returning to app retains the draft');
  fs.writeFileSync(path.join(out, 'links-report.json'), JSON.stringify({ status: 'passed', serial, checks,
    fixture: 'reserved example origins/documentation IP, CDP-intercepted HTML, actual adb taps',
    realModelCalls: 0, authentication: 'not attempted', browserDispatch: 'ACTION_VIEW host/port observed; no real private-network download attempted' }, null, 2));
})().catch(error => {
  console.error('Link smoke failed; inspect ignored local artifacts (not for publication).');
  fs.writeFileSync(path.join(out, 'links-failure.txt'), String(error.stack));
  try { fs.writeFileSync(path.join(out, 'links-failure.xml'), dump()); shot('failure'); } catch {}
  process.exitCode = 1;
}).finally(async () => {
  if (browser) await browser.close().catch(() => {});
  if (port) try { adb('forward', '--remove', `tcp:${port}`); } catch {}
});
