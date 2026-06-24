// Peek into the live sprite using the corrected exec wire format.
const fs = require('fs');
const WebSocket = require('ws');
const env = fs.readFileSync('../.env','utf8');
const get = k => { const l = env.split('\n').find(x=>x.startsWith(k+'=')); return l ? l.split('=').slice(1).join('=').replace(/^"|"$/g,'').trim() : ''; };
const TOKEN = get('SPRITES_TOKEN');
const WSS='wss://api.sprites.dev/v1';
const name = process.argv[2] || 'flaborful-fb5d904d1a4b488e';

function exec(script){return new Promise((res,rej)=>{
  const params=[['cmd','/bin/bash'],['cmd','-c'],['cmd',script],['max_run_after_disconnect','30s']];
  const qs=params.map(([k,v])=>`${encodeURIComponent(k)}=${encodeURIComponent(v)}`).join('&');
  const ws=new WebSocket(`${WSS}/sprites/${encodeURIComponent(name)}/exec?${qs}`,{headers:{Authorization:`Bearer ${TOKEN}`}});
  let out='',err='',code=null;
  ws.on('message',d=>{const b=Buffer.isBuffer(d)?d:Buffer.from(d);const id=b[0],p=b.slice(1);if(id===1)out+=p;else if(id===2)err+=p;else if(id===3)code=p.length===4?p.readInt32BE(0):p[0];});
  ws.on('close',()=>res({out:out.toString(),err:err.toString(),code}));
  ws.on('error',rej);setTimeout(()=>{try{ws.close()}catch{};res({out:out.toString(),err:err.toString(),code,note:'to'})},25000);
});}

(async()=>{
  const checks = {
    'repo cloned?': "ls -la /workspace/repo 2>&1 | head -20",
    'git status/branch': "cd /workspace/repo 2>/dev/null && git status -sb 2>&1 | head -20 || echo 'no repo dir'",
    'claude process alive?': "ps aux | grep -E 'claude -p' | grep -v grep | head -3 | cut -c1-160; echo '(truncated)'",
    'whats running (top cpu)': "ps -eo pid,pcpu,comm --sort=-pcpu | head -8",
    'recent file changes': "cd /workspace/repo 2>/dev/null && git diff --stat 2>&1 | tail -20 || echo 'n/a'",
    'system load / uptime': "uptime",
  };
  for(const [label,script] of Object.entries(checks)){
    const e=await exec(script);
    console.log(`\n===== ${label} (exit ${e.code}) =====`);
    if(e.out.trim()) console.log(e.out.trim());
    if(e.err.trim()) console.log('[stderr]', e.err.trim());
  }
})().catch(e=>{console.error('ERR',e.message);process.exit(1)});
