// x * 1103515245 exceeds 2^53, where a number stops being exact; the remainder mod
// 2^31 depends only on the low 32 bits, which Math.imul computes exactly.
const n = Number(process.argv[2]);
const xs: number[] = [];
let x = 12345;
for (let i = 0; i < n; i++) {
  x = ((Math.imul(x, 1103515245) + 12345) | 0) & 0x7fffffff;
  xs.push(x);
}
xs.sort((a, b) => a - b);
console.log(xs[0] + xs[Math.floor(xs.length / 2)] + xs[xs.length - 1]);

export {};
