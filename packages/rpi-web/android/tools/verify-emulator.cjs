/* Local emulator only. Never logs in or calls real model/command endpoints. */
const { chromium } = require('playwright');
const { execFileSync } = require('node:child_process');
const fs = require('node:fs');
const path = require('node:path');
const assert = require('node:assert/strict');
const serial = process.env.ANDROID_SERIAL || 'emulator-5584';
if (!serial.startsWith('emulator-')) throw new Error('This test is restricted to emulators, not user phones.');
const app = 'cn.bigfisharc.rpiweb.local';
const out = path.resolve(__dirname, '../artifacts');
fs.mkdirSync(out, { recursive: true });
const adb = (...args) => execFileSync('adb', ['-s', serial, ...args], { encoding: 'utf8', timeout: 30000 });
const sleep = ms => new Promise(resolve => setTimeout(resolve, ms));
let browser, port;
const checks = [];
function ok(name) { checks.push(name); console.log('PASS', name); }
function dump() {
  adb('shell', 'rm', '-f', '/sdcard/rpi-shell-ui.xml');
  try { adb('shell', 'uiautomator', 'dump', '/sdcard/rpi-shell-ui.xml'); }
  catch (error) { if (!String(error.stdout).includes('UI hierchary dumped to:')) throw error; }
  const xml = adb('shell', 'cat', '/sdcard/rpi-shell-ui.xml');
  assert.ok(xml.includes('<hierarchy') && xml.includes('</hierarchy>'));
  return xml;
}
function tapText(text) {
  const xml = dump();
  const node = [...xml.matchAll(/<node\b[^>]*>/g)].map(m => m[0]).find(n =>
    n.includes(`text="${text}"`) || n.includes(`content-desc="${text}"`));
  if (!node) { fs.writeFileSync(path.join(out, 'missing-ui.xml'), xml); throw new Error(`Native UI item missing: ${text}`); }
  const [, x1, y1, x2, y2] = node.match(/bounds="\[(\d+),(\d+)\]\[(\d+),(\d+)\]"/);
  adb('shell', 'input', 'tap', String(Math.floor((+x1 + +x2) / 2)), String(Math.floor((+y1 + +y2) / 2)));
}
function screenshot(name) {
  fs.writeFileSync(path.join(out, name), execFileSync('adb', ['-s', serial, 'exec-out', 'screencap', '-p'], { timeout: 30000 }));
}
async function connect() {
  const pid = adb('shell', 'pidof', app).trim().split(' ')[0];
  if (browser) await browser.close().catch(() => {});
  if (port) adb('forward', '--remove', `tcp:${port}`);
  port = adb('forward', 'tcp:0', `localabstract:webview_devtools_remote_${pid}`).trim();
  browser = await chromium.connectOverCDP(`http://127.0.0.1:${port}`, { timeout: 20000, noDefaults: true });
  for (let n = 0; n < 30; n++) {
    const page = browser.contexts()[0]?.pages().find(p => p.url().startsWith('https://'));
    if (page) { page.setDefaultTimeout(15000); return page; }
    await sleep(300);
  }
  throw new Error('No HTTPS WebView target');
}
async function site(name, origin) {
  tapText('站点设置'); await sleep(300);
  tapText(name); await sleep(300);
  tapText('切换'); await sleep(2500);
  const page = await connect();
  assert.equal(new URL(page.url()).origin, origin);
  return page;
}
(async () => {
  adb('shell', 'am', 'force-stop', app);
  adb('shell', 'am', 'start', '-n', `${app}/.MainActivity`);
  await sleep(5000);
  let page = await connect();
  // Reset to production if a previous interrupted run left a different preference.
  if (new URL(page.url()).origin !== 'https://work.bigfisharc.cn') page = await site('正式站', 'https://work.bigfisharc.cn');
  await page.locator('input[type=password]').waitFor({ state: 'visible' });
  screenshot('production-login.png');
  ok('Real production HTTPS login loads without credentials');

  await page.locator('input[type=password]').click();
  await sleep(1200);
  const ime = adb('shell', 'dumpsys', 'input_method');
  assert.match(ime, /mInputShown=true|isInputViewShown=true|mIsInputViewShown=true/);
  screenshot('keyboard-login.png');
  adb('shell', 'input', 'keyevent', '4'); await sleep(800);
  assert.match(adb('shell', 'dumpsys', 'activity', 'activities'), /cn\.bigfisharc\.rpiweb\.local\/\.MainActivity/);
  ok('Native soft keyboard opens; Back dismisses IME without leaving app');

  page = await site('开发站（测试界面）', 'https://work-dev.bigfisharc.cn');
  await page.locator('input[type=password]').waitFor({ state: 'visible' });
  screenshot('development-login.png');
  ok('Explicit native site switch loads real development HTTPS login');
  adb('shell', 'am', 'force-stop', app);
  adb('shell', 'am', 'start', '-n', `${app}/.MainActivity`);
  await sleep(2500); page = await connect();
  assert.equal(new URL(page.url()).origin, 'https://work-dev.bigfisharc.cn');
  ok('Selected site persists across app process restart');

  // All following HTML is intercepted only in this emulator WebView. No fixture
  // endpoint or command is sent to the shared service or persisted transcript.
  const fixture = 'https://work-dev.bigfisharc.cn/__local_android_shell_test__';
  const html = `<!doctype html><meta name="viewport" content="width=device-width, initial-scale=1"><title>Local shell fixture</title>
<style>body{font:16px sans-serif;padding:16px}button,input,textarea{display:block;margin:16px 0;min-height:44px}textarea{width:90%}</style>
<h1>Local shell fixture</h1><input id="files" type="file" multiple><output id="selected"></output>
<button id="popup" onclick="window.open('https://example.com/actual-destination','_blank')">Open external popup</button>
<a id="unsafe" href="javascript:document.title='unsafe-ran'">Script URL (never clicked)</a>
<a id="internal" href="?page=2">Internal next page</a>
<div style="height:40vh"></div><textarea aria-label="Draft" placeholder="Draft"></textarea>
<script>files.onchange=()=>{selected.textContent=[...files.files].map(f=>f.name).join(',')};</script>`;
  await page.route('**/__local_android_shell_test__*', route => route.fulfill({ status: 200, contentType: 'text/html', body: html }));
  await page.goto(fixture);
  await page.locator('h1').waitFor();
  await page.locator('#internal').click();
  await page.waitForURL(`${fixture}?page=2`);
  adb('shell', 'input', 'keyevent', '4');
  await page.waitForURL(fixture);
  ok('System Back navigates trusted WebView history');

  await page.locator('#popup').click(); await sleep(700);
  const popupXml = dump();
  assert.ok(popupXml.includes('example.com'));
  assert.ok(popupXml.includes('在系统浏览器中打开'));
  assert.equal(new URL(page.url()).origin, 'https://work-dev.bigfisharc.cn');
  tapText('取消');
  ok('Actual script popup URL is confirmed externally, never loaded into trusted view');

  await page.locator('textarea').click(); await sleep(700);
  screenshot('keyboard-fixture.png');
  const metrics = await page.locator('textarea').evaluate(el => ({ bottom: el.getBoundingClientRect().bottom, height: innerHeight }));
  assert.ok(metrics.bottom <= metrics.height + 2, JSON.stringify(metrics));
  adb('shell', 'input', 'keyevent', '4'); await sleep(400);
  ok('Fixture composer remains visible above the native keyboard');

  const sample = path.join(out, 'rpi-android-fixture.txt');
  fs.writeFileSync(sample, 'Local shell fixture; no backend upload');
  adb('push', sample, '/sdcard/Download/rpi-android-fixture.txt');
  adb('shell', 'am', 'broadcast', '-a', 'android.intent.action.MEDIA_SCANNER_SCAN_FILE', '-d', 'file:///sdcard/Download/rpi-android-fixture.txt');
  await page.locator('#files').click(); await sleep(1000);
  let picker = dump();
  fs.writeFileSync(path.join(out, 'picker-ui.xml'), picker);
  screenshot('system-file-picker.png');
  assert.match(adb('shell', 'dumpsys', 'activity', 'activities'), /documentsui/);
  // DocumentsUI may start in Recents. Navigate to Downloads if needed.
  if (!picker.includes('rpi-android-fixture.txt')) {
    tapText('Show roots'); await sleep(300); tapText('Downloads'); await sleep(700);
  }
  tapText('rpi-android-fixture.txt'); await sleep(400);
  // Multiple selection mode has a final OPEN/SELECT action on some versions.
  if (!adb('shell', 'dumpsys', 'activity', 'top').includes('cn.bigfisharc.rpiweb.local/.MainActivity')) {
    const current = dump();
    for (const label of ['OPEN', 'Open', 'SELECT', 'Select']) if (current.includes(`text="${label}"`)) { tapText(label); break; }
  }
  await page.locator('#selected').filter({ hasText: 'rpi-android-fixture.txt' }).waitFor();
  ok('System document picker returns a real content URI to the HTML file input');
  await page.locator('#files').click(); await sleep(500); adb('shell', 'input', 'keyevent', '4');
  await page.locator('#files').click(); await sleep(500);
  assert.match(adb('shell', 'dumpsys', 'activity', 'activities'), /documentsui/);
  adb('shell', 'input', 'keyevent', '4');
  ok('Cancelling the system picker permits reopening it');

  page = await site('正式站', 'https://work.bigfisharc.cn');
  await page.locator('input[type=password]').waitFor({ state: 'visible' });
  ok('Site switch discards fixture history and restores the default production connection');
  fs.writeFileSync(path.join(out, 'emulator-report.json'), JSON.stringify({ status: 'passed', serial, android: adb('shell', 'getprop', 'ro.build.version.release').trim(), checks, authentication: 'not attempted; no server credentials read', realModelCalls: 0, fixture: 'CDP intercepted HTML only; no fixture backend or persisted session' }, null, 2));
})().catch(error => { console.error(error); screenshot('test-failure.png'); process.exitCode = 1; }).finally(async () => {
  if (browser) await browser.close().catch(() => {});
  if (port) { try { adb('forward', '--remove', `tcp:${port}`); } catch {} }
});
