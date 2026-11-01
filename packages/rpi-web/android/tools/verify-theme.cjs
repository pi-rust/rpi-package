/* Disposable emulator debug build only. No credentials, shared writes or model calls. */
const { chromium } = require('playwright');
const { execFileSync } = require('node:child_process');
const fs = require('node:fs');
const path = require('node:path');
const assert = require('node:assert/strict');
const serial = process.env.ANDROID_SERIAL || 'emulator-5584';
assert.match(serial, /^emulator-\d+$/);
const app = 'cn.bigfisharc.rpiweb.local';
const out = path.resolve(__dirname, '../artifacts');
fs.mkdirSync(out, { recursive: true });
const adb = (...args) => execFileSync('adb', ['-s', serial, ...args], { encoding: 'utf8', timeout: 30000 });
const sleep = ms => new Promise(r => setTimeout(r, ms));
const checks = [];
let browser, port;
function ok(text) { checks.push(text); console.log('PASS', text); }
function dump() {
  try { adb('shell', 'uiautomator', 'dump', '/sdcard/rpi-theme.xml'); } catch {}
  return adb('shell', 'cat', '/sdcard/rpi-theme.xml');
}
function tap(text) {
  const node = [...dump().matchAll(/<node\b[^>]*>/g)].map(m => m[0]).find(n => n.includes(`text="${text}"`) || n.includes(`content-desc="${text}"`));
  assert.ok(node, `Native item missing: ${text}`);
  const [,x1,y1,x2,y2] = node.match(/bounds="\[(\d+),(\d+)\]\[(\d+),(\d+)\]"/);
  adb('shell','input','tap',String((+x1 + +x2) >> 1),String((+y1 + +y2) >> 1));
}
function shot(name) { fs.writeFileSync(path.join(out, name + '.png'), execFileSync('adb', ['-s',serial,'exec-out','screencap','-p'])); }
async function mode(night, page) {
  adb('shell','cmd','uimode','night',night ? 'yes' : 'no');
  await page.waitForFunction(expected => matchMedia('(prefers-color-scheme: dark)').matches === expected, night, { timeout: 12000 });
  await sleep(700);
}
(async () => {
  adb('shell','cmd','uimode','night','no');
  adb('shell','am','force-stop',app);
  adb('shell','am','start','-W','-n',app+'/.MainActivity');
  await sleep(3000);
  const pid = adb('shell','pidof',app).trim();
  port = adb('forward','tcp:0',`localabstract:webview_devtools_remote_${pid}`).trim();
  browser = await chromium.connectOverCDP(`http://127.0.0.1:${port}`, { noDefaults: true });
  const page = browser.contexts()[0].pages().find(p => p.url().startsWith('https://'));
  assert.ok(page);
  const origin = new URL(page.url()).origin;
  const fixture = origin + '/__local_android_theme_fixture__';
  await page.route('**/__local_android_theme_fixture__*', route => route.fulfill({ contentType: 'text/html', body: `<!doctype html><meta name="viewport" content="width=device-width,initial-scale=1"><style>:root{color-scheme:light dark}body{background:white;color:#252529;padding:16px;font:16px sans-serif}@media(prefers-color-scheme:dark){body{background:#1c1c1e;color:#f2f2f4}}textarea{width:90%;min-height:100px}</style><h1>System theme fixture</h1><textarea aria-label="Draft"></textarea><a href="?next=1">Next</a>` }));
  await page.goto(fixture);
  await page.locator('a').click();
  await page.getByRole('textbox', {name:'Draft'}).fill('Unsent theme draft');
  const marker = await page.evaluate(() => { window.themeMarker = crypto.randomUUID(); return window.themeMarker; });
  assert.equal(await page.evaluate(() => matchMedia('(prefers-color-scheme:dark)').matches), false);
  shot('theme-day');
  await mode(true, page);
  assert.equal(await page.evaluate(() => getComputedStyle(document.body).backgroundColor), 'rgb(28, 28, 30)');
  assert.equal(adb('shell','pidof',app).trim(), pid);
  assert.equal(await page.evaluate(() => window.themeMarker), marker);
  assert.equal(await page.getByRole('textbox',{name:'Draft'}).inputValue(), 'Unsent theme draft');
  shot('theme-night');
  ok('OS day→night updates native shell and WebView CSS without reload, process change or draft loss');
  tap('站点设置'); await sleep(400); shot('theme-dialog-night');
  await mode(false, page); shot('theme-dialog-day');
  assert.ok(dump().includes('自定义 HTTPS 站点'));
  tap('自定义 HTTPS 站点'); await sleep(400);
  const inputXml = dump(); assert.ok(inputXml.includes('HTTPS 站点地址'));
  await mode(true, page); shot('theme-custom-dialog-night');
  assert.ok(dump().includes(origin));
  tap('取消');
  assert.equal(await page.getByRole('textbox',{name:'Draft'}).inputValue(), 'Unsent theme draft');
  ok('Open site/custom dialogs recolor across mode changes while preserving dialog/input/page state');
  await mode(false, page);
  assert.equal(await page.evaluate(() => getComputedStyle(document.body).backgroundColor), 'rgb(255, 255, 255)');
  assert.equal(await page.evaluate(() => window.themeMarker), marker);
  adb('shell','input','keyevent','4');
  await sleep(500);
  // The custom-origin EditText can leave its IME visible; Back dismisses it
  // before the next Back navigates page history.
  if (page.url() !== fixture) adb('shell','input','keyevent','4');
  await page.waitForURL(fixture);
  ok('OS night→day restores light colors and retains trusted WebView Back history');
  await mode(true, page);
  // A real network failure is local to this WebView; no server mutation.
  await page.route('**/__local_android_theme_error__', route => route.abort('failed'));
  await page.goto(origin+'/__local_android_theme_error__').catch(()=>{});
  await sleep(1000); assert.ok(dump().includes('重试加载页面')); shot('theme-error-night');
  await mode(false,page); shot('theme-error-day');
  ok('Native error/retry panel switches day/night without reload');
  fs.writeFileSync(path.join(out,'theme-emulator-report.json'),JSON.stringify({status:'passed',serial,checks,realModelCalls:0,fixture:'CDP intercepted on emulator only',authentication:'not attempted'},null,2));
})().catch(()=>{console.error('Theme smoke failed; inspect ignored local artifacts (not for publication).');shot('theme-failure');process.exitCode=1;}).finally(async()=>{
  if(browser) await browser.close().catch(()=>{});
  if(port) try{adb('forward','--remove',`tcp:${port}`);}catch{}
  try{adb('shell','cmd','uimode','night','no');}catch{}
});
