import assert from 'node:assert/strict';
import {randomUUID} from 'node:crypto';
// Check the installed relay using isolated fixture accounts. No saved outfit is published.
const root='http://127.0.0.1:38261';
const pair=await (await fetch(root+'/v1/pair')).json();
const clients=[];
async function connect(userId,role){
 const socket=new WebSocket(root.replace('http:','ws:')+'/v1/socket');const messages=[];const waiters=[];
 const result={socket,messages,next:predicate=>new Promise((resolve,reject)=>{
  const found=messages.find(predicate);if(found)return resolve(found);
  const timer=setTimeout(()=>reject(Error('Timed out waiting for avatar relay')),3000);waiters.push({predicate,resolve:value=>{clearTimeout(timer);resolve(value)}});
 })};clients.push(result);
 socket.addEventListener('message',event=>{const value=JSON.parse(event.data);messages.push(value);for(const waiter of [...waiters])if(waiter.predicate(value)){waiters.splice(waiters.indexOf(waiter),1);waiter.resolve(value)}});
 socket.addEventListener('open',()=>socket.send(JSON.stringify({type:'auth',version:1,token:pair.code,userId,role,avatarLookup:role==='game'})));
 const ready=await result.next(m=>m.type==='ready');assert.equal(ready.avatarLookup,true,'desktop must advertise native lookup support');return result;
}
try{
 const game=await connect('7','game'),browser=await connect('7','browser'),other=await connect('8','game');
 const requestId=randomUUID();browser.socket.send(JSON.stringify({type:'avatar-lookup',id:requestId,targetUserId:'99'}));
 const request=await game.next(m=>m.type==='avatar-lookup');assert.equal(request.targetUserId,'99');assert.notEqual(request.id,requestId);
 assert.equal(other.messages.some(m=>m.type==='avatar-lookup'),false);
 const snapshot={version:1,userId:'7',items:[{id:123,name:'Fixture hat',itemType:'Asset',assetType:8}],definition:{assets:[{id:123}],scales:{width:.8},playerAvatarType:{playerAvatarType:'R6'},bodyColors:Object.fromEntries(['head','torso','leftArm','rightArm','leftLeg','rightLeg'].map(p=>[p+'Color','ffffff']))}};
 game.socket.send(JSON.stringify({type:'avatar-lookup-result',id:request.id,ok:true,snapshot}));
 const response=await browser.next(m=>m.type==='avatar-lookup-result'&&m.id===requestId);assert.equal(response.ok,true);assert.equal(response.snapshot.definition.playerAvatarType.playerAvatarType,'R6');assert.equal(response.snapshot.items[0].id,123);
 assert.equal(response.targetUserId,'99');assert.equal(browser.messages.some(m=>m.channel==='game'&&m.value?.items?.[0]?.id===123),false,'lookup cannot equip its result');
 const second=randomUUID();browser.socket.send(JSON.stringify({type:'avatar-lookup',id:second,targetUserId:'99'}));await game.next(m=>m.type==='avatar-lookup'&&m.id!==request.id);game.socket.close();
 const interrupted=await browser.next(m=>m.id===second);assert.equal(interrupted.ok,false);assert.match(interrupted.error,/disconnected/);
 console.log('PASS: installed desktop relays correlated avatar data, isolates accounts, leaves saved outfits untouched, and reports client disconnect.');
}finally{for(const client of clients)client.socket.close()}
