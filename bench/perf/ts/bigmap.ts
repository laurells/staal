const n = Number(process.argv[2]);
const m = new Map<number, number>([[0, 0]]);
for (let i = 1; i < n; i++) m.set((i * 7919) % 1000003, i);
let s = 0;
for (let j = 0; j < n; j++) {
  const v = m.get((j * 7919) % 1000003);
  s += v === undefined ? -1 : v;
}
console.log(s);

export {};
