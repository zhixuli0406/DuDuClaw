#!/usr/bin/env node
// Actual RFB authentication in a fresh, isolated local image. Requires the
// dashboard's installed noVNC package. No account, model or network is used.
import assert from 'node:assert/strict';
import {spawn, spawnSync} from 'node:child_process';
import {once} from 'node:events';
import {mkdtemp, writeFile} from 'node:fs/promises';
import {tmpdir} from 'node:os';
import {join} from 'node:path';
import {randomBytes, randomUUID} from 'node:crypto';
import {DESECBCipher} from '../../web/node_modules/@novnc/novnc/core/crypto/des.js';

const image = process.argv[2];
assert.ok(image && !image.startsWith('-'), 'provide an already-built local image');
const nonce = randomUUID();
const name = `dudu-vnc-test-${nonce}`;
const home = await mkdtemp(join(tmpdir(), 'dudu-vnc-auth-'));
console.log(`ARTIFACTS ${home}`);
const docker = (args, input, check = true) => {
  const result = spawnSync('docker', args, {input, encoding:'utf8', timeout:35000, maxBuffer:4*1024*1024});
  if (check) assert.equal(result.status, 0, result.stderr || result.error?.message);
  return result;
};
let container;
const owned = () => container && docker(['inspect','-f','{{index .Config.Labels "duduclaw.vnc-test"}}',container]).stdout.trim() === nonce;

async function authenticate(password, shouldPass) {
  const child = spawn('docker', ['exec','-i',container,'duduclaw-vnc-relay']);
  const closed = once(child, 'close');
  let pending = Buffer.alloc(0), waiter, ended = false;
  child.on('error', () => {ended=true; waiter?.();});
  child.stdout.on('data', data => {
    pending = Buffer.concat([pending, data]);
    if (pending.length > 1024*1024) {ended=true; child.kill();}
    waiter?.();
  });
  child.stdout.on('end', () => {ended=true; waiter?.();});
  child.stderr.resume();
  const read = async length => {
    if (pending.length < length && !ended) await new Promise((resolve, reject) => {
      const timer = setTimeout(() => {waiter=null; reject(Error('RFB read timed out'));}, 5000);
      waiter = () => {
        if (pending.length >= length || ended) {clearTimeout(timer); waiter=null; resolve();}
      };
      waiter();
    });
    assert.ok(pending.length >= length, 'RFB server closed before authentication completed');
    const result = pending.subarray(0,length); pending=pending.subarray(length); return result;
  };
  try {
    assert.equal((await read(12)).toString(), 'RFB 003.008\n');
    child.stdin.write('RFB 003.008\n');
    const count=(await read(1))[0]; assert.ok(count);
    assert.ok((await read(count)).includes(2), 'VNC password authentication required');
    child.stdin.write(Buffer.from([2]));
    const challenge=await read(16);
    const key=DESECBCipher.importKey([...password].map(c=>c.charCodeAt(0)));
    child.stdin.write(key.encrypt({},challenge));
    const result=(await read(4)).readUInt32BE();
    if (!shouldPass) {assert.notEqual(result,0,'wrong password must be refused'); return;}
    assert.equal(result,0,'actual VNC authentication');
    child.stdin.write(Buffer.from([1]));
    const init=await read(24);
    assert.equal(init.readUInt16BE(0),1280); assert.equal(init.readUInt16BE(2),800);
  } finally {
    if (child.exitCode === null && child.signalCode === null) child.kill();
    await closed;
  }
}

try {
  container=docker(['run','-d','--name',name,'--pull','never','--label',`duduclaw.vnc-test=${nonce}`,
    '--read-only','--tmpfs','/tmp:rw,nosuid,size=256m','--security-opt','no-new-privileges',
    '--cpus','1','--memory','512m','--pids-limit','512','--network','none',
    '-e','DISPLAY=:99','-e','DISPLAY_SIZE=1280x800',image]).stdout.trim();
  assert.ok(owned());
  let ready=false;
  for(let n=0;n<100;n++) {
    if(docker(['exec',container,'xdotool','getactivewindow'],undefined,false).status===0) {ready=true;break;}
    await new Promise(resolve=>setTimeout(resolve,200));
  }
  assert.ok(ready,'actual browser readiness');
  const password=randomBytes(8).toString('hex').slice(0,8);
  docker(['exec','-i',container,'duduclaw-vnc','start','viewonly'],password+'\n');
  await authenticate(password,true);
  await authenticate(password,true); // Reconnect after the rm: password file was consumed.
  await authenticate('WrongKey',false);
  const socket=JSON.parse(docker(['exec',container,'python3','-c',
    "import json,os,stat; p='/tmp/duduclaw-root/vnc.sock'; s=os.stat(p); r=os.stat(os.path.dirname(p)); print(json.dumps({'uid':s.st_uid,'mode':stat.S_IMODE(s.st_mode),'root_mode':stat.S_IMODE(r.st_mode)}))"]).stdout);
  assert.deepEqual(socket,{uid:0,mode:0o600,root_mode:0o700});
  assert.notEqual(docker(['exec','-u','sandbox',container,'python3','-c',
    "import socket; s=socket.socket(socket.AF_UNIX); s.connect('/tmp/duduclaw-root/vnc.sock')"],undefined,false).status,0);
  const listeners=docker(['exec',container,'python3','-c',
    "import json; a=[]\nfor p in ['/proc/net/tcp','/proc/net/tcp6']:\n for l in open(p).readlines()[1:]:\n  f=l.split(); port=int(f[1].split(':')[-1],16)\n  if f[3]=='0A' and 5900<=port<=5999:a.append(port)\nprint(json.dumps(a))"]).stdout;
  assert.deepEqual(JSON.parse(listeners),[]);
  docker(['exec',container,'duduclaw-vnc','stop']);
  assert.notEqual(docker(['exec',container,'test','-e','/tmp/duduclaw-root/vnc.sock'],undefined,false).status,0);
  const result={status:'PASS',image,image_id:docker(['image','inspect','-f','{{.Id}}',image]).stdout.trim(),
    authentication:true,reconnect:true,wrong_password_denied:true,sandbox_denied:true,no_vnc_tcp:true,stop_removed_socket:true};
  await writeFile(join(home,'result.json'),JSON.stringify(result,null,2)); console.log(JSON.stringify(result));
} finally {
  if (container && owned()) {
    await writeFile(join(home,'container.inspect.json'),docker(['inspect',container]).stdout);
    await writeFile(join(home,'container.log'),docker(['logs',container],undefined,false).stdout);
    docker(['rm','-f',container]);
  }
}
