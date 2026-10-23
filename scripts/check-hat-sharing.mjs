import assert from 'node:assert/strict';
import { randomUUID } from 'node:crypto';

// Live bridge/relay smoke check. Isolated test accounts and a unique test room.
const pair = await (await fetch('http://127.0.0.1:38261/v1/pair')).json();
const room = randomUUID();
const clients = [];
function connect(userId) {
  const socket = new WebSocket('ws://127.0.0.1:38261/v1/socket');
  const messages = [];
  socket.addEventListener('message', event => messages.push(JSON.parse(event.data)));
  socket.addEventListener('open', () => socket.send(JSON.stringify({type:'auth',version:1,token:pair.code,userId,role:'game'})));
  const client = {socket,messages}; clients.push(client); return client;
}
async function until(predicate) {
  const end = Date.now()+10000;
  while (!predicate()) {
    assert.ok(Date.now()<end,'Timed out waiting for shared hat state');
    await new Promise(resolve=>setTimeout(resolve,20));
  }
}
function record(userId) {
  return {version:1,userId,name:{enabled:false,username:'',displayName:''},snapshot:{
    version:1,userId,items:[],definition:{assets:[],bodyColors:Object.fromEntries(
      ['head','torso','leftArm','rightArm','leftLeg','rightLeg'].map(region=>[region+'Color','ffffff'])),scales:{},playerAvatarType:{playerAvatarType:'R15'}}},
    hatScripts:{version:1,session:randomUUID(),held:'',events:[]}};
}
function share(client,value) {
  client.socket.send(JSON.stringify({type:'sharing',value:{enabled:true,placeId:'123',jobId:room,record:value}}));
}
const first=connect('7'), second=connect('8');
try {
  await until(()=>clients.every(client=>client.messages.some(message=>message.type==='ready')));
  const source=record('7'); share(first,source); share(second,record('8'));
  const shared = client => client.messages.filter(message=>message.channel==='shared').map(message=>message.value);
  await until(()=>shared(second).some(event=>event.type==='snapshot'&&event.players.some(value=>value.userId==='7')||event.type==='upsert'&&event.record.userId==='7'));
  source.hatScripts.held='award';
  source.hatScripts.events=[{id:1,kind:'award-burst',at:Math.floor(Date.now()/1000)}];
  share(first,source);
  await until(()=>shared(second).some(event=>event.type==='hat-scripts'&&event.userId==='7'));
  const event=shared(second).find(event=>event.type==='hat-scripts'&&event.userId==='7');
  assert.deepEqual(event.hatScripts,source.hatScripts);
  assert.equal(event.snapshot,undefined); assert.equal(event.record,undefined);
  console.log('PASS: installed desktop → live relay → peer desktop delivers compact hat actions without resending avatars.');
} finally {
  for (const client of clients) client.socket.close();
}
