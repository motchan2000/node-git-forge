// usage: node ab.mjs port1,port2,... -> alternate blocks of ops per server; median block mean per op
const ports = process.argv[2].split(',');
const H={Authorization:'Bearer tk','Content-Type':'application/json'};
const body=JSON.stringify({title:'Some issue title [bench 1]',body:'A body text '.repeat(20)});
const S = {};
for (const p of ports) { const B='http://127.0.0.1:'+p;
  await fetch(B+'/api/v3/user/repos',{method:'POST',headers:H,body:JSON.stringify({name:'mb'})});
  const r=await (await fetch(B+'/api/v3/repos/adaiversen/mb/issues',{method:'POST',headers:H,body})).json();
  S[p]={B,n:r.number,res:{get:[],post:[]}}; }
const ops = {
  get: async s => { const x=await fetch(s.B+'/api/v3/repos/adaiversen/mb/issues/'+s.n,{headers:H}); await x.json(); },
  post: async s => { const x=await fetch(s.B+'/api/v3/repos/adaiversen/mb/issues',{method:'POST',headers:H,body}); await x.json(); },
};
const blocks = +(process.argv[3]||12), per = 100;
for (let b=0;b<blocks;b++) for (const op of Object.keys(ops)) for (const p of (b%2?ports.slice().reverse():ports)) {
  const s=S[p]; for(let i=0;i<10;i++) await ops[op](s);
  const t=performance.now(); for(let i=0;i<per;i++) await ops[op](s); s.res[op].push((performance.now()-t)/per*1000);
}
const med=a=>{a=[...a].sort((x,y)=>x-y);return a[a.length>>1].toFixed(0)};
for (const p of ports) console.log(p, 'get', med(S[p].res.get), 'post', med(S[p].res.post));
