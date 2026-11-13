const n = Number(process.argv[2]);
const counts = new Map<number, number>([[0, 0]]);
for (let i = 0; i < n; i++) {
  const k = (i * 7919) % 1000;
  counts.set(k, (counts.get(k) ?? 0) + 1);
}
let s = 0;
for (const [k, v] of counts) s += k * v;
console.log(s);

export {};
