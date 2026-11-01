'use strict';
const $=id=>document.getElementById(id);
$('username').value=localStorage.getItem('furbo-last-username')||'';
const t=(key,values)=>I18n.t(key,values);
if(location.pathname==='/')history.replaceState({},'','/overview'+location.search);
const pagePaths={home:'/overview',media:'/recordings',schedule:'/feed-times',settings:'/settings',connections:'/connections',admin:'/accounts'};
const state={token:'',principal:null,status:null,schedules:[],media:[],view:Object.keys(pagePaths).find(key=>pagePaths[key]===location.pathname)||'home',videoAbort:null,recording:null,previewUrl:null,setupOnly:false,videoWanted:false};
const node=(tag,text,cls)=>{const e=document.createElement(tag);if(text!==undefined)e.textContent=t(String(text));if(cls)e.className=cls;return e;};
const action=(label,fn)=>{const b=node('button',label);b.type='button';b.addEventListener('click',guard(fn));return b;};
function notice(text){clearTimeout(state.noticeTimer);const close=action('×',()=>{$('notice').hidden=true;});close.setAttribute('aria-label',t('Dismiss'));$('notice').replaceChildren(node('span',text),close);$('notice').hidden=false;state.noticeTimer=setTimeout(()=>{$('notice').hidden=true;},7000);}
function guard(fn){return async e=>{try{await fn(e);}catch(err){notice(err.message||String(err));}};}
async function api(path,opts={},publicRequest=false){
  const headers=new Headers(opts.headers);
  if(!publicRequest)headers.set('Authorization','Bearer '+state.token);
  let body=opts.body;
  if(body!==undefined&&!(body instanceof Blob)&&!(body instanceof ArrayBuffer)){headers.set('Content-Type','application/json');body=JSON.stringify(body);}
  const response=await fetch('/api/v1'+path,{...opts,headers,body,cache:'no-store'});
  if(!response.ok){let msg='Request failed ('+response.status+')';try{const v=await response.json();msg=v.error||msg;}catch{}if(response.status===401&&!publicRequest)disconnect();throw new Error(msg);}
  return opts.raw?response:response.json();
}
function disconnect(){stopVideo();if(state.recording)state.recording(true).catch(()=>{});closeMenu();document.body.dataset.authenticated='false';sessionStorage.removeItem('furbo-session');state.token='';state.principal=null;$('shell').hidden=true;$('login').hidden=false;$('password').value='';$('api-key').value='';}
async function connect(resumeToken){
  const key=resumeToken||$('api-key').value.trim();state.token=key;
  if(!key){if(!$('username').value.trim())throw new Error(t('Enter your username.'));const v=await api('/login',{method:'POST',body:{username:$('username').value.trim(),password:$('password').value}},true);state.token=v.token;state.setupOnly=!!v.setup_only;if(v.setup_only)notice('The clock is unset. Use Wi-Fi setup or sync the clock, then sign in again.');}
  state.principal=await api('/me');localStorage.setItem('furbo-last-username',state.principal.username);$('username').value=state.principal.username;sessionStorage.setItem('furbo-session',JSON.stringify({token:state.token,setupOnly:state.setupOnly}));$('password').value='';$('api-key').value='';
  document.body.dataset.authenticated='true';$('login').hidden=true;$('shell').hidden=false;view(state.view,false);
  document.querySelectorAll('.admin-only').forEach(e=>e.hidden=state.principal.role!=='admin');
  document.querySelectorAll('.control').forEach(e=>e.disabled=state.principal.role==='viewer');
  await refresh();await refreshLux();if(state.setupOnly){view('admin');await loadAdmin();return;}await loadMedia();await loadSchedules();if(state.principal.role==='admin')await loadAdmin();if(state.view==='connections')await loadSettings();
}
$('login-form').addEventListener('submit',async e=>{e.preventDefault();$('login-error').textContent='';try{await connect();}catch(err){$('login-error').textContent=err.message;state.token='';}});
$('logout').addEventListener('click',guard(async()=>{try{await api('/logout',{method:'POST'});}finally{disconnect();}}));
function view(name,push=true){
  closeMenu();if(name!=='home')stopVideo();
  if(!pagePaths[name])name='home';
  if(state.principal&&state.principal.role!=='admin'&&['connections','admin'].includes(name))name='home';
  if(push&&(location.pathname!==pagePaths[name]||new URLSearchParams(location.search).has('lang')))history.pushState({},'',pagePaths[name]);
  state.view=name;document.querySelectorAll('[data-page]').forEach(e=>e.hidden=e.dataset.page!==name);
  document.querySelectorAll('[data-view]').forEach(e=>e.classList.toggle('selected',e.dataset.view===name));
  $('page-title').textContent=t({home:'Overview',media:'Recordings',schedule:'Feed times',settings:'Device settings',connections:'Connections & events',admin:'Accounts & network'}[name]);
}
document.querySelectorAll('[data-view]').forEach(b=>b.addEventListener('click',guard(async()=>{view(b.dataset.view);if(state.view==='media')await loadMedia();if(state.view==='schedule')await loadSchedules();if(state.view==='admin')await loadAdmin();if(state.view==='connections')await loadSettings();})));
async function refresh(){
  if(!state.token)return;
  state.status=await api('/status');const s=state.status;
  $('device-name').textContent=s.device+(s.simulated?' · simulation':'');
  $('clock-warning').hidden=s.clock_valid;$('timezone').textContent=s.timezone;
  $('last-feed').textContent=s.last_feed?t('Last toss: {time}',{time:new Date(s.last_feed*1000).toLocaleString(I18n.locale)}):t('No treats tossed yet.');
  const imageControls=s.capabilities.image_controls||[];
  for(const option of $('image-setting').options)option.disabled=!imageControls.includes(option.value);
  if($('image-setting').selectedOptions[0]?.disabled)$('image-setting').value=imageControls[0]||'anti_flicker';
  $('volume').value=s.volume;$('volume-value').textContent=s.volume+'%';$('mute').checked=s.muted;$('night').value=s.night;
  const stats=await api('/stats');$('health').textContent=JSON.stringify(stats,null,2);
  const format=n=>n==null?t('Not reported'):n>=1073741824?(n/1073741824).toFixed(2)+' GiB':(n/1048576).toFixed(1)+' MiB';
  const summary=$('stats-summary');summary.replaceChildren();
  const metrics=[['MAC address',stats.device?.mac_address||'Not reported'],['Serial number',stats.device?.serial_number||'Not reported'],['RAM available',format(stats.memory?.available_bytes)+' / '+format(stats.memory?.total_bytes)],['API memory',format(stats.process?.rss_bytes)]];
  for(const disk of (stats.storage||[]).slice(0,2))metrics.push([t('Free space: {path}',{path:disk.path}),format(disk.available_bytes)+' / '+format(disk.total_bytes)]);
  for(const [label,value]of metrics){const item=node('div');item.append(node('span',label,'small muted'),node('strong',value));summary.append(item);}
  if(!state.setupOnly)renderEvents(await api('/events'));
}
setInterval(()=>{if(state.token)refresh().catch(()=>{});},20000);
async function command(actionBody){
  const result=await api('/commands',{method:'POST',body:{id:crypto.randomUUID(),issued_at:Math.floor(Date.now()/1000),action:actionBody}});
  if(!result.ok)throw new Error(result.error||'Command failed');
  await refresh();return result.result;
}
$('toss').addEventListener('click',guard(async()=>{if(confirm('Is the treat path clear? Toss one treat now?')){await command({type:'toss'});notice('Treat cycle completed.');}}));
$('snapshot').addEventListener('click',guard(async()=>{notice('Taking a photo…');const r=await command({type:'capture',upload:false});await loadMedia();await preview(r);notice('Photo saved on the device.');}));
$('record-video').addEventListener('click',guard(async()=>{notice('Recording video and room audio for up to 10 seconds…');await command({type:'video',seconds:10,upload:false});await loadMedia();notice('Video saved on the device.');}));
$('device-record').addEventListener('click',guard(async()=>{notice('Recording the room microphone for 5 seconds…');await command({type:'record',seconds:5,upload:false});await loadMedia();notice('Room audio saved.');}));
$('read-lux').addEventListener('click',guard(refreshLux));
$('night').addEventListener('change',guard(()=>command({type:'night',mode:$('night').value})));
$('volume').addEventListener('input',()=>$('volume-value').textContent=$('volume').value+'%');
$('volume').addEventListener('change',guard(()=>command({type:'volume',percent:Number($('volume').value)})));
$('mute').addEventListener('change',guard(()=>command({type:'mute',muted:$('mute').checked})));
$('tunnel').addEventListener('change',guard(()=>command({type:'tunnel',on:$('tunnel').checked})));
document.querySelectorAll('[data-color]').forEach(b=>b.addEventListener('click',guard(()=>command({type:'led',color:b.dataset.color}))));
$('apply-image').addEventListener('click',guard(()=>command({type:'image',setting:$('image-setting').value,value:Number($('image-value').value)})));
$('zoom').addEventListener('input',()=>$('video').style.transform='scale('+$('zoom').value+')');
function stopVideo(){state.videoWanted=false;state.videoAbort?.abort();state.videoAbort=null;$('video').hidden=true;$('video').pause();$('video').removeAttribute('src');$('video').load();$('camera-placeholder').hidden=false;$('live-state').textContent='OFF';}
$('stop-video').addEventListener('click',stopVideo);
$('start-video').addEventListener('click',guard(startVideo));
$('video-audio').addEventListener('change',guard(async()=>{
  $('video').muted=!$('video-audio').checked;
  if(state.videoWanted){
    $('video').play().catch(()=>{});
    await startVideo();
  }
}));
async function startVideo(){
  stopVideo();state.videoWanted=true;if(!window.MediaSource)throw new Error('This browser needs MediaSource support for H.264 live video.');
  const controller=new AbortController();state.videoAbort=controller;
  const response=await api('/stream?audio='+$('video-audio').checked,{raw:true,signal:controller.signal});
  const mime=response.headers.get('Content-Type');if(!MediaSource.isTypeSupported(mime))throw new Error('This browser cannot play '+mime+'. Use a browser with H.264/AAC MediaSource support.');
  const source=new MediaSource();const url=URL.createObjectURL(source);$('video').hidden=false;$('video').src=url;$('video').muted=!$('video-audio').checked;
  await new Promise(resolve=>source.addEventListener('sourceopen',resolve,{once:true}));URL.revokeObjectURL(url);
  const buffer=source.addSourceBuffer(mime);const reader=response.body.getReader();$('camera-placeholder').hidden=true;$('live-state').textContent='LIVE';
  const append=data=>new Promise((resolve,reject)=>{const done=()=>{buffer.removeEventListener('error',fail);resolve();};const fail=e=>{buffer.removeEventListener('updateend',done);reject(e);};buffer.addEventListener('updateend',done,{once:true});buffer.addEventListener('error',fail,{once:true});try{buffer.appendBuffer(data);}catch(e){buffer.removeEventListener('updateend',done);buffer.removeEventListener('error',fail);reject(e);}});
  $('video').play().catch(()=>{if(!controller.signal.aborted)notice('Press play in the video controls to allow playback.');});
  try{
    while(!controller.signal.aborted){
      const {done,value}=await reader.read();if(done)break;
      if(buffer.buffered.length&&$('video').currentTime>25){
        buffer.remove(0,$('video').currentTime-15);await new Promise(resolve=>buffer.addEventListener('updateend',resolve,{once:true}));
      }
      await append(value);
      if(buffer.buffered.length){const end=buffer.buffered.end(buffer.buffered.length-1);if(end-$('video').currentTime>4)$('video').currentTime=Math.max(0,end-1);}
    }
    if(source.readyState==='open')source.endOfStream();
  }catch(e){if(e.name!=='AbortError')throw e;}
  finally{reader.cancel().catch(()=>{});if(state.videoAbort===controller){state.videoAbort=null;$('live-state').textContent='ENDED';if(state.videoWanted&&state.token)setTimeout(()=>startVideo().catch(e=>notice(e.message)),1000);}}
}
async function loadMedia(){
  state.media=await api('/media');const list=$('media-list');list.replaceChildren();
  if(!state.media.length)list.append(node('p','Nothing saved yet. Take a photo or record a voice message to begin.','muted'));
  const select=$('schedule-voice');const chosen=select.value;select.replaceChildren(new Option('None',''));
  for(const m of [...state.media].reverse()){
    const row=node('div',undefined,'row'),box=node('div');
    box.append(node('strong',m.content_type+' · '+Math.ceil(m.bytes/1024)+' KiB'),node('p',new Date(m.created*1000).toLocaleString(I18n.locale)+' · '+m.id));row.append(box);
    const actions=node('div',undefined,'row-actions');actions.append(action('Open',()=>preview(m)),action('Download',()=>download(m)));
    if(m.content_type==='audio/wav')select.add(new Option(m.id,m.id));
    if(state.principal.role!=='viewer'){
      if(m.content_type==='audio/wav')actions.append(action('Play on Furbo',()=>command({type:'play',media_id:m.id})));
      actions.append(action('Upload',async()=>{await command({type:'upload',media_id:m.id});notice('Queued for your configured upload endpoint.');}));
      actions.append(action('Delete',async()=>{if(confirm('Delete this recording from the device?')){await api('/media/'+m.id,{method:'DELETE'});await loadMedia();}}));
    }
    row.append(actions);list.append(row);
  }
  if([...select.options].some(o=>o.value===chosen))select.value=chosen;
}
async function blobFor(m){return(await api('/media/'+m.id,{raw:true})).blob();}
async function preview(m){
  const blob=await blobFor(m);if(state.previewUrl)URL.revokeObjectURL(state.previewUrl);state.previewUrl=URL.createObjectURL(blob);
  const e=node(m.content_type.startsWith('image/')?'img':m.content_type.startsWith('audio/')?'audio':'video');
  e.src=state.previewUrl;if(e.tagName==='IMG')e.alt='Captured photo';else e.controls=true;
  $('preview-content').replaceChildren(e);$('preview').showModal();
}
$('close-preview').addEventListener('click',()=>{$('preview').close();$('preview-content').replaceChildren();if(state.previewUrl)URL.revokeObjectURL(state.previewUrl);state.previewUrl=null;});
async function download(m){const url=URL.createObjectURL(await blobFor(m)),a=node('a');a.href=url;a.download=m.id+({'image/jpeg':'.jpg','audio/wav':'.wav','video/mp4':'.mp4'}[m.content_type]||'');a.click();setTimeout(()=>URL.revokeObjectURL(url),10000);}
$('voice-file').addEventListener('change',guard(async e=>{const file=e.target.files[0];if(file){await api('/voice',{method:'PUT',body:file});await loadMedia();notice('Voice message saved.');e.target.value='';}}));
function wav(samples,rate){
  const n=Math.floor(samples.length*16000/rate),b=new ArrayBuffer(44+n*2),v=new DataView(b);
  const text=(o,s)=>[...s].forEach((c,i)=>v.setUint8(o+i,c.charCodeAt(0)));text(0,'RIFF');v.setUint32(4,36+n*2,true);text(8,'WAVEfmt ');v.setUint32(16,16,true);v.setUint16(20,1,true);v.setUint16(22,1,true);v.setUint32(24,16000,true);v.setUint32(28,32000,true);v.setUint16(32,2,true);v.setUint16(34,16,true);text(36,'data');v.setUint32(40,n*2,true);
  for(let i=0;i<n;i++){const sample=Math.max(-1,Math.min(1,samples[Math.floor(i*rate/16000)]));v.setInt16(44+i*2,sample<0?sample*32768:sample*32767,true);}return b;
}
$('talk').addEventListener('click',guard(async()=>{
  if(state.recordingPending)return;
  if(state.recording){await state.recording();return;}
  if(!navigator.mediaDevices)throw new Error('Browser microphone access requires HTTPS.');
  state.recordingPending=true;$('talk').disabled=true;$('talk').textContent='Starting microphone…';
  let stream,context;
  try{
    context=new AudioContext();await context.resume();
    stream=await navigator.mediaDevices.getUserMedia({audio:true});
    if(!state.token)throw new Error('Recording cancelled.');
    const input=context.createMediaStreamSource(stream),processor=context.createScriptProcessor(4096,1,1),gain=context.createGain();gain.gain.value=0;
    const parts=[];let count=0;
    processor.onaudioprocess=e=>{const b=e.inputBuffer.getChannelData(0);if(count<context.sampleRate*15){parts.push(new Float32Array(b));count+=b.length;}};
    input.connect(processor);processor.connect(gain);gain.connect(context.destination);
    let timer;
    const stop=async(cancel=false)=>{
      if(state.recording!==stop)return;
      state.recording=null;clearTimeout(timer);processor.disconnect();input.disconnect();stream.getTracks().forEach(t=>t.stop());
      const samples=new Float32Array(Math.min(count,context.sampleRate*15));let at=0;
      for(const part of parts){const chunk=part.subarray(0,samples.length-at);samples.set(chunk,at);at+=chunk.length;if(at>=samples.length)break;}
      const bytes=wav(samples,context.sampleRate);await context.close();$('talk').textContent='Record a voice message';
      if(cancel)return;
      if(!samples.length)throw new Error('No microphone audio arrived. Try recording again.');
      const media=await api('/voice',{method:'PUT',body:bytes});await command({type:'play',media_id:media.id});await loadMedia();notice('Voice message played and saved.');
    };
    state.recording=stop;
    timer=setTimeout(()=>stop().catch(e=>notice(e.message)),15000);
    $('talk').textContent='Stop & play voice message';notice('Recording your browser microphone. Click again to stop (maximum 15 seconds).');
  }catch(error){
    stream?.getTracks().forEach(t=>t.stop());if(context&&context.state!=='closed')await context.close();throw error;
  }finally{
    state.recordingPending=false;$('talk').disabled=state.principal?.role==='viewer';
    if(!state.recording)$('talk').textContent='Record a voice message';
  }
}));

async function loadSchedules(){
  state.schedules=await api('/schedules');const list=$('schedule-list');list.replaceChildren();
  if(!state.schedules.length)list.append(node('p','No feed times yet.','muted'));
  for(const s of state.schedules){
    const row=node('div',undefined,'row'),box=node('div');box.append(node('strong',s.time+(s.enabled?'':' · '+t('Paused'))),node('p',s.days.map(d=>t(['Mon','Tue','Wed','Thu','Fri','Sat','Sun'][d])).join(' · ')+(s.voice_id?' · '+t('with voice message'):'')));
    const buttons=node('div',undefined,'row-actions');if(state.principal.role!=='viewer'){
      buttons.append(action(s.enabled?'Pause':'Enable',async()=>{s.enabled=!s.enabled;await saveSchedules();}),action('Remove',async()=>{state.schedules=state.schedules.filter(v=>v.id!==s.id);await saveSchedules();}));
    }row.append(box,buttons);list.append(row);
  }
}
async function saveSchedules(){await api('/schedules',{method:'PUT',body:state.schedules});await loadSchedules();}
$('schedule-form').addEventListener('submit',guard(async e=>{e.preventDefault();state.schedules.push({id:crypto.randomUUID().replaceAll('-','').slice(0,16),time:$('feed-time').value,days:[...$('days').querySelectorAll('input:checked')].map(e=>Number(e.value)),enabled:true,voice_id:$('schedule-voice').value||null});await saveSchedules();notice('Feed time saved.');}));
async function loadAdmin(){
  if(state.principal.role!=='admin')return;
  if(state.setupOnly){await loadNetwork();return;}
  const users=await api('/users');$('user-list').replaceChildren();$('key-user').replaceChildren();
  for(const u of users){
    $('key-user').add(new Option(u.username,u.username));const row=node('div',undefined,'row');
    row.append(node('strong',u.username+' · '+t(u.role==='admin'?'Administrator':u.role==='viewer'?'View only':'Device controls')+(u.disabled?' · '+t('disabled'):'')));
    if(u.username==='admin'&&!u.disabled)row.append(node('span','Default administrator','activity-result'));
    else row.append(action(u.disabled?'Enable':'Disable',async()=>{await api('/users/'+u.username,{method:'PUT',body:{disabled:!u.disabled}});await loadAdmin();}));
    $('user-list').append(row);
  }
  const keys=await api('/keys');$('key-list').replaceChildren();
  for(const k of keys){const row=node('div',undefined,'row');row.append(node('strong',k.name+' · '+k.username),action('Revoke',async()=>{if(confirm('Revoke this API key?')){await api('/keys/'+k.id,{method:'DELETE'});await loadAdmin();}}));$('key-list').append(row);}
  await loadNetwork();
}
$('user-form').addEventListener('submit',guard(async e=>{e.preventDefault();await api('/users',{method:'POST',body:{username:$('new-username').value,password:$('new-password').value,role:$('new-role').value}});$('new-password').value='';await loadAdmin();notice('Local account created.');}));
$('key-form').addEventListener('submit',guard(async e=>{e.preventDefault();const result=await api('/keys',{method:'POST',body:{username:$('key-user').value,name:$('key-name').value}});$('new-key').textContent=result.key;await loadAdmin();}));
$('wifi-form').addEventListener('submit',guard(async e=>{e.preventDefault();if(!confirm('Save this Wi-Fi and reconnect? Your current connection will close.'))return;const trial=await api('/network',{method:'PUT',body:{ssid:$('wifi-ssid').value,passphrase:$('wifi-password').value}});$('wifi-password').value='';$('wifi-transaction').value=trial.transaction_id;$('wifi-confirm-panel').hidden=false;notice('Wi-Fi trial started. Confirm the transaction within five minutes; a reboot rolls it back.');}));
$('recovery').addEventListener('click',guard(async()=>{if(confirm('Switch to recovery Wi-Fi? This connection will close.')){await api('/network/recover',{method:'POST'});notice('Switching to recovery Wi-Fi.');}}));
$('sync-clock').addEventListener('click',guard(async()=>{await api('/time',{method:'POST',body:{unix_seconds:Math.floor(Date.now()/1000)}});if(state.setupOnly){const v=await api('/session/upgrade',{method:'POST'});state.token=v.token;state.setupOnly=false;sessionStorage.setItem('furbo-session',JSON.stringify({token:state.token,setupOnly:false}));await loadMedia();await loadSchedules();await loadAdmin();}notice('Clock synchronized.');await refresh();}));
window.addEventListener('beforeunload',()=>state.videoAbort?.abort());

async function loadNetwork(){
  try{
    const value=await api('/network');$('network-state').textContent=JSON.stringify(value,null,2);
    $('wifi-confirm-panel').hidden=!value.pending;
    if(value.transaction_id)$('wifi-transaction').value=value.transaction_id;
    $('confirm-wifi').disabled=!(value.mode==='trial'&&value.connected);
  }catch(e){$('network-state').textContent=e.message;}
}
setInterval(()=>{if(state.token&&state.view==='admin'&&state.principal?.role==='admin')loadNetwork().catch(()=>{});},5000);
$('confirm-wifi').addEventListener('click',guard(async()=>{await api('/network/confirm',{method:'POST',body:{transaction_id:$('wifi-transaction').value}});notice('Wi-Fi confirmed and saved.');await loadNetwork();}));
$('rollback-wifi').addEventListener('click',guard(async()=>{await api('/network/rollback',{method:'POST'});notice('Returning to the last confirmed Wi-Fi.');}));
async function checkSetup(){
  const result=await api('/setup/status',{},true);if(result.required)sessionStorage.removeItem('furbo-session');$('login-form').hidden=result.required;$('setup-form').hidden=!result.required;
}
$('setup-form').addEventListener('submit',async e=>{
  e.preventDefault();$('setup-error').textContent='';
  try{
    const password=$('setup-password').value;
    if(password!==$('setup-confirm').value)throw new Error('Passwords do not match.');
    await api('/setup/password',{method:'POST',body:{password,unix_seconds:Math.floor(Date.now()/1000)}},true);
    $('setup-password').value='';$('setup-confirm').value='';await checkSetup();
    $('login-error').textContent='Password saved. Sign in as admin.';
  }catch(e){$('setup-error').textContent=e.message;}
});
async function startup(){
  await I18n.ready;await checkSetup();
  const saved=JSON.parse(sessionStorage.getItem('furbo-session')||'null');
  if(saved&&!$('login-form').hidden){state.setupOnly=!!saved.setupOnly;try{await connect(saved.token);}catch{disconnect();}}
}
I18n.onChange=()=>{view(state.view,false);updateThemeButton();if(state.token){refresh().catch(()=>{});loadMedia().catch(()=>{});loadSchedules().catch(()=>{});}};
startup().catch(e=>$('login-error').textContent=e.message);

async function refreshLux(){
  if(!state.token||state.setupOnly)return;
  const value=await api('/sensors');$('lux').textContent=value.lux==null?'—':Number(value.lux).toFixed(1);
}
setInterval(()=>{if(state.token&&!document.hidden)refreshLux().catch(()=>{});},5000);
function activityTitle(event){
  const a=event.details?.action;
  if(!a)return t({capture:'Photo captured',audio_recording:'Room audio recorded',video_recording:'Video recorded',uploaded:'Media uploaded',motion:'Motion detected',sound:'Sound detected',retention:'Recording cleanup',command:'Device command'}[event.kind]||event.kind.replaceAll('_',' '));
  const types={toss:'Treat tossed',led:'Indicator: {value}',night:'Night vision: {value}',volume:'Speaker: {value}%',mute:'Speaker mute: {value}',tunnel:'Tunnel lamp: {value}',image:'Camera setting: {value}',wifi_stage:'Wi-Fi trial started',wifi_confirm:'Wi-Fi confirmed',wifi_rollback:'Wi-Fi rolled back',set_schedules:'Feed times updated',delete_media:'Recording deleted',play:'Voice message played',upload:'Media upload queued',configure:'Settings updated'};
  const value=a.color??a.mode??a.percent??(a.muted==null?(a.on==null?a.setting:(a.on?'On':'Off')):(a.muted?'On':'Off'));
  return t(types[a.type]||'Device command',{value:t(String(value??''))});
}
function renderEvents(events){
  const list=$('event-list');list.replaceChildren();
  const visible=events.filter(e=>!(e.kind==='command'&&['capture','record','video'].includes(e.details?.action?.type)&&e.details?.response?.ok)).slice(-12).reverse();
  if(!visible.length)list.append(node('p','Your next photo, treat, or device event will appear here.','muted small'));
  for(const event of visible){
    const response=event.details?.response||event.details;
    const failed=response?.ok===false;
    const row=node('div',undefined,'activity-row'),box=node('div',undefined,'activity-detail');
    const icon=node('span',event.kind==='capture'?'▧':event.kind.includes('recording')?'▷':failed?'!':'•','activity-icon');
    box.append(node('strong',activityTitle(event)),node('p',new Date(event.timestamp*1000).toLocaleString(I18n.locale)));
    if(failed)box.append(node('p',response.error||t('Command failed')));
    row.append(icon,box);
    if(event.details?.content_type&&event.details?.id)row.append(action('Open',()=>preview(event.details)));
    else row.append(node('span',failed?'Failed':'Done','activity-result'+(failed?' failed':'')));
    list.append(row);
  }
}
async function loadSettings(){
  if(state.principal?.role!=='admin'||state.setupOnly)return;
  state.settings=await api('/settings');const c=state.settings.config;
  const values={
    'mqtt-host':c.mqtt.host,'mqtt-port':c.mqtt.port,'mqtt-username':c.mqtt.username||'','mqtt-topic':c.mqtt.topic_prefix,'mqtt-cert-name':c.mqtt.certificate_name||'',
    'http-listen':c.http.listen,'https-listen':c.http.https_listen||'','http-concurrency':c.http.max_connections,
    'webhook-url':c.media.webhook_url||'','upload-url':c.media.upload_url||'',
    'motion-threshold':c.detection.motion_threshold,'sound-threshold':c.detection.sound_threshold,'detect-interval':c.detection.interval_seconds,'detect-cooldown':c.detection.cooldown_seconds,
    'media-quota':c.media.max_bytes/1048576,'media-files':c.media.max_files,'media-days':c.media.retention_days,'media-free':c.media.min_free_bytes/1048576,
    'watchdog-audio':c.watchdog.audio_cpu_percent,'watchdog-rtsp':c.watchdog.rtsp_cpu_percent,'watchdog-readings':c.watchdog.consecutive_readings,
    'overlay-text':c.camera.overlay_text,'settings-timezone':c.timezone
  };
  for(const [id,value]of Object.entries(values))$(id).value=value;
  const checks={'mqtt-enabled':c.mqtt.enabled,'mqtt-tls':c.mqtt.tls,'mqtt-selfsigned':c.mqtt.allow_self_signed||!!c.mqtt.ca_file,'http-lan':c.http.http_enabled!==false&&c.http.allow_insecure_lan,'https-enabled':c.http.https_enabled!==false&&!!c.http.https_listen,'auto-upload':c.media.auto_upload,'endpoint-selfsigned':c.media.allow_self_signed||!!c.media.ca_file,'webhook-selfsigned':c.media.webhook_allow_self_signed||!!c.media.ca_file,'detect-motion':c.detection.motion,'detect-sound':c.detection.sound,'detect-capture':c.detection.capture_on_event,'overlay-time':c.camera.show_timestamp};
  for(const [id,value]of Object.entries(checks))$(id).checked=value;
  const events=c.media.webhook_events||[];$('webhook-filter').value=events.length===0?'all':events.length===5&&events.includes('motion')?'detection':events.length===4&&events.includes('capture')?'media':'custom';
  const host=location.hostname,port=address=>address.match(/:(\d+)$/)?.[1];$('listener-urls').textContent=(c.http.http_enabled!==false&&c.http.allow_insecure_lan?'HTTP: http://'+host+':'+port(c.http.listen)+'/overview · ':'')+(c.http.https_enabled!==false&&c.http.https_listen?'HTTPS: https://'+host+':'+port(c.http.https_listen)+'/overview':'');
}
$('connection-form').addEventListener('submit',guard(async event=>{
  event.preventDefault();if(!state.settings)await loadSettings();
  const c=structuredClone(state.settings.config),value=id=>$(id).value.trim(),number=id=>Number(value(id)),checked=id=>$(id).checked;
  Object.assign(c.mqtt,{enabled:checked('mqtt-enabled'),tls:checked('mqtt-tls'),allow_self_signed:checked('mqtt-selfsigned'),host:value('mqtt-host'),port:number('mqtt-port'),username:value('mqtt-username')||null,password:$('mqtt-password').value||null,certificate_name:value('mqtt-cert-name')||null,topic_prefix:value('mqtt-topic')});
  if(c.mqtt.password)c.mqtt.password_file=null;
  let httpAddress=value('http-listen');if(checked('http-lan')&&httpAddress.startsWith('127.0.0.1:'))httpAddress=httpAddress.replace('127.0.0.1:','0.0.0.0:');if(!checked('http-lan'))httpAddress='127.0.0.1:'+httpAddress.split(':').at(-1);
  Object.assign(c.http,{listen:httpAddress,https_listen:value('https-listen')||'0.0.0.0:8443',http_enabled:checked('http-lan'),https_enabled:checked('https-enabled'),allow_insecure_lan:checked('http-lan'),max_connections:number('http-concurrency')});
  Object.assign(c.media,{upload_url:value('upload-url')||null,webhook_url:value('webhook-url')||null,auto_upload:checked('auto-upload'),allow_self_signed:checked('endpoint-selfsigned'),webhook_allow_self_signed:checked('webhook-selfsigned'),max_bytes:Math.round(number('media-quota')*1048576),max_files:number('media-files'),retention_days:number('media-days'),min_free_bytes:Math.round(number('media-free')*1048576)});
  Object.assign(c.detection,{motion:checked('detect-motion'),sound:checked('detect-sound'),motion_threshold:number('motion-threshold'),sound_threshold:number('sound-threshold'),interval_seconds:number('detect-interval'),cooldown_seconds:number('detect-cooldown'),capture_on_event:checked('detect-capture')});
  Object.assign(c.camera,{overlay_text:value('overlay-text'),show_timestamp:checked('overlay-time')});
  const filter=$('webhook-filter').value;if(filter!=='custom')c.media.webhook_events=filter==='detection'?['motion','sound','bark','person','dog']:filter==='media'?['capture','video_recording','audio_recording','uploaded']:[];
  if(value('webhook-url')&&!state.settings.secrets.webhook_secret&&!$('webhook-secret').value){$('webhook-secret').closest('details').open=true;$('webhook-secret').focus();throw new Error(t('Set a signing secret in Callback authentication.'));}
  const body={mqtt:c.mqtt,http:c.http,media:c.media,detection:c.detection,camera:c.camera,watchdog:{audio_cpu_percent:number('watchdog-audio'),rtsp_cpu_percent:number('watchdog-rtsp'),consecutive_readings:number('watchdog-readings')},timezone:value('settings-timezone')};
  for(const [id,key]of [['mqtt-ca','mqtt_ca_pem'],['endpoint-ca','endpoint_ca_pem'],['upload-token','upload_token'],['webhook-secret','webhook_secret']])if($(id).value)body[key]=$(id).value;
  const cert=$('https-cert').files[0],key=$('https-key').files[0];
  if(cert)body.server_certificate_pem=await cert.text();if(key)body.server_private_key_pem=await key.text();
  const result=await api('/settings',{method:'PUT',body});
  for(const id of ['mqtt-password','mqtt-ca','endpoint-ca','upload-token','webhook-secret','https-cert','https-key'])$(id).value='';
  stopVideo();$('settings-result').textContent=t('Saved. The service is restarting.');
  const address=result.listeners.https||result.listeners.http;
  if(!address){$('settings-result').textContent=t('Web listeners disabled. Use MQTT or hold reset for 10 seconds to restore access.');return;}
  const match=address.match(/:(\d+)$/),next=new URL(location.href);
  next.protocol=result.listeners.https?'https:':'http:';next.port=match?match[1]:next.port;
  if(next.origin!==location.origin){
    const link=node('a',t('Open the new dashboard address'));link.href=next.href;$('settings-result').append(' ',link);return;
  }
  await new Promise(resolve=>setTimeout(resolve,3000));
  for(let attempt=0;attempt<20;attempt++){
    try{await api('/me');await loadSettings();await refresh();$('settings-result').textContent=t('Settings applied.');return;}catch(error){if(!state.token)throw error;await new Promise(resolve=>setTimeout(resolve,1000));}
  }
  throw new Error(t('Settings saved. Reconnect when the service is available.'));
}));
$('download-log').addEventListener('click',guard(async()=>{
  const response=await api('/logs.ndjson',{raw:true}),url=URL.createObjectURL(await response.blob()),link=node('a');link.href=url;link.download='furbo-log.ndjson';link.click();setTimeout(()=>URL.revokeObjectURL(url),10000);
}));

window.addEventListener('popstate',()=>{
  const name=Object.keys(pagePaths).find(key=>pagePaths[key]===location.pathname)||'home';
  I18n.fromUrl?.();view(name,false);
  if(state.token){
    if(name==='connections')loadSettings().catch(e=>notice(e.message));
    if(name==='admin')loadAdmin().catch(e=>notice(e.message));
    if(name==='media')loadMedia().catch(e=>notice(e.message));
    if(name==='schedule')loadSchedules().catch(e=>notice(e.message));
  }
});

function closeMenu(){document.body.classList.remove('menu-open');$('menu-backdrop').hidden=true;$('menu-toggle').setAttribute('aria-expanded','false');}
$('menu-toggle').addEventListener('click',()=>{const open=document.body.classList.toggle('menu-open');$('menu-backdrop').hidden=!open;$('menu-toggle').setAttribute('aria-expanded',String(open));});
$('menu-backdrop').addEventListener('click',closeMenu);document.addEventListener('keydown',event=>{if(event.key==='Escape')closeMenu();});
function updateThemeButton(){const dark=document.documentElement.dataset.theme==='dark';$('theme-toggle').innerHTML=dark?"<svg viewBox=\"0 0 24 24\" aria-hidden=\"true\"><circle cx=\"12\" cy=\"12\" r=\"4\" fill=\"currentColor\"/><path d=\"M12 2v2m0 16v2M2 12h2m16 0h2M4.9 4.9l1.4 1.4m11.4 11.4 1.4 1.4M4.9 19.1l1.4-1.4M17.7 6.3l1.4-1.4\" fill=\"none\" stroke=\"currentColor\" stroke-width=\"1.8\" stroke-linecap=\"round\"/></svg>" : "<svg viewBox=\"0 0 24 24\" aria-hidden=\"true\"><path d=\"M20.9 13A9 9 0 0 1 11 3.1 9 9 0 1 0 20.9 13Z\" fill=\"none\" stroke=\"currentColor\" stroke-width=\"1.8\" stroke-linecap=\"round\" stroke-linejoin=\"round\"/></svg>";$('theme-toggle').setAttribute('aria-label',t(dark?'Switch to light mode':'Switch to dark mode'));$('theme-toggle').title=t(dark?'Switch to light mode':'Switch to dark mode');}
function setTheme(theme){document.documentElement.dataset.theme=theme;localStorage.setItem('furbo-theme',theme);updateThemeButton();}
$('theme-toggle').addEventListener('click',()=>setTheme(document.documentElement.dataset.theme==='dark'?'light':'dark'));
setTheme(localStorage.getItem('furbo-theme')||(matchMedia('(prefers-color-scheme: dark)').matches?'dark':'light'));

$('forget-certs').addEventListener('click',guard(async()=>{
  await api('/settings',{method:'PUT',body:{reset_tls_pins:true}});
  notice('Remembered certificates cleared. The service is restarting.');
}));
