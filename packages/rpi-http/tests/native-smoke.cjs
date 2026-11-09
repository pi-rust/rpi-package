// Actual existing rpi ABI, isolated data, no external model calls or Work owner.
const assert = require('node:assert/strict');
const {spawn} = require('node:child_process');
const fs = require('node:fs');
const http = require('node:http');
const os = require('node:os');
const path = require('node:path');

function bounded(promise, ms = 20000) {
  return Promise.race([promise, new Promise((_, reject) => {
    const timer = setTimeout(() => reject(new Error('Native HTTP smoke timeout')), ms);
    timer.unref();
  })]);
}

(async () => {
  const rpi = process.env.RPI_NATIVE_BIN;
  const library = process.env.RPI_HTTP_EXTENSION;
  assert(rpi && library, 'Set RPI_NATIVE_BIN and RPI_HTTP_EXTENSION to built files');
  const root = fs.mkdtempSync(path.join(os.tmpdir(), 'rpi-http-api-only-'));
  fs.mkdirSync(path.join(root, 'agent'));
  fs.mkdirSync(path.join(root, 'project'));
  let modelCalls = 0;
  const model = http.createServer((req, res) => { modelCalls++; res.writeHead(500); res.end(); });
  await new Promise(resolve => model.listen(0, '127.0.0.1', resolve));
  const config = path.join(root, 'http.json');
  fs.writeFileSync(config, JSON.stringify({project:'project', registry:'sessions.json', bind:'127.0.0.1', port:0}));
  const env = {...process.env, RPI_CODING_AGENT_DIR:path.join(root, 'agent'),
    OPENAI_API_KEY:'isolated-bootstrap-only', RPI_HTTP_PASSWORD:'isolated-http-only'};
  for (const key of ['RPI_HTTP_ALLOWED_HOSTS','RPI_WEB_ALLOWED_HOSTS','RPI_WEB_PASSWORD',
    'RPI_NODE_CONTROL_STDIN','RPI_NODE_CONTROL_TOKEN']) delete env[key];
  const child = spawn(rpi, ['--approve','--local-only','--extension',library,`--http=${config}`,
    '--no-session','--no-skills','--no-prompt-templates','--no-context-files',
    '--provider','openai','--model','gpt-6-astra','--base-url',`http://127.0.0.1:${model.address().port}/v1`],
    {cwd:root,env,windowsHide:true,stdio:['pipe','pipe','pipe']});
  const exited = new Promise(resolve => child.once('exit', resolve));
  let output = '', errors = '', finished = false;
  child.stderr.on('data', bytes => {errors += bytes;});
  try {
    const address = await bounded(new Promise((resolve, reject) => {
      child.once('error', reject);
      child.once('exit', code => reject(new Error(`HTTP plugin exited ${code}: ${errors}`)));
      child.stdout.on('data', bytes => {
        output += bytes;
        const found = output.match(/rpi HTTP API listening on (127\.0\.0\.1:\d+)/);
        if (found) resolve(`http://${found[1]}/`);
      });
    })).catch(error => {throw new Error(`${error.message}\nstdout: ${output.slice(-3000)}\nstderr: ${errors.slice(-3000)}`);});
    assert.equal((await fetch(address+'api/v1/health')).status, 200);
    assert.equal((await fetch(address+'api/v1/projects')).status, 401);
    const login = await fetch(address+'api/v1/auth', {method:'POST',
      headers:{'content-type':'application/json','x-rpi-web':'1'},
      body:JSON.stringify({password:'isolated-http-only'})});
    assert.equal(login.status, 200);
    const cookie = login.headers.get('set-cookie').split(';')[0];
    assert.equal((await fetch(address+'api/v1/projects',{headers:{cookie}})).status, 200);
    assert.equal((await fetch(address+'api/v1/components',{headers:{cookie}})).status, 404);
    assert.equal((await fetch(address)).status, 404);
    assert.equal(modelCalls, 0);
    // This smoke verifies the real installed ABI and API boundary. The existing
    // host's service mode stops on Ctrl+C; stdin EOF is not a shutdown command.
    // Terminate only this isolated smoke process (no Agent children were run).
    child.kill();
    await bounded(exited);
    finished = true;
    await assert.rejects(fetch(address+'api/v1/health'));
    console.log('PASS original rpi ABI, API-only HTTP/auth, no Work owner/routes, zero model calls, isolated cleanup');
  } finally {
    if (!finished) {child.stdin.end();child.kill();await bounded(exited,5000).catch(()=>{});}
    await new Promise(resolve => model.close(resolve));
  }
})().catch(error => {console.error(error);process.exitCode=1;});
