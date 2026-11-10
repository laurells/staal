// Writes src/jcs_vectors.json: doubles, each given by its IEEE 754 bits, with the text
// ECMAScript's Number::toString gives for it, as computed by this Node.js (String(x)).
// src/jcs.rs tests its own number formatting against these. The doubles are chosen to
// cover every branch of Number::toString (integers, the point inside the digits, small
// numbers, both exponent forms) and their boundaries, plus random bit patterns.
//
//   node scripts/jcs_vectors.mjs > src/jcs_vectors.json
//
// The random part uses a fixed-seed generator (xorshift64*), so the file is the same on
// every run of the same Node.js.
const buf = new DataView(new ArrayBuffer(8));
const bitsOf = (x) => { buf.setFloat64(0, x); return buf.getBigUint64(0).toString(16).padStart(16, "0"); };
const fromBits = (b) => { buf.setBigUint64(0, b); return buf.getFloat64(0); };

const xs = [];
const add = (x) => { if (Number.isFinite(x)) xs.push(x); };
// Branch boundaries and well-known edges.
for (const x of [0, -0, 1, -1, 0.1, 0.2, 0.3, 1 / 3, 2 / 3, Math.PI, Math.E, 5e-324, -5e-324,
  Number.MAX_VALUE, -Number.MAX_VALUE, Number.MIN_VALUE, Number.MAX_SAFE_INTEGER, Number.MAX_SAFE_INTEGER + 1,
  2 ** 53, 2 ** 63, 2 ** 64, 2 ** 70, 1e21, 1e21 - 65536, 999999999999999900000, 1e20, 1e22, 1e23,
  9.999999999999997e+22, 1.0000000000000001e+23, 1e-6, 1e-7, 9.999999999999997e-7, 0.000001, 1.5e-7,
  123e-20, 123e20, 4.5, 0.002, 1e30, 333333333.3333332, 1424953923781206.2, -0.0000033333333333333333]) add(x);
for (let e = -330; e <= 310; e++) { add(Number(`1e${e}`)); add(Number(`1.5e${e}`)); add(Number(`9.87654321e${e}`)); }
for (let n = 0; n < 64; n++) { add(2 ** n); add(2 ** n - 1); add(-(2 ** n)); }
// Neighbours of the branch boundaries (n = 21, n = -6), by bit pattern.
for (const x of [1e21, 1e-6, 1e-7, 1e20]) {
  const b = BigInt("0x" + bitsOf(x));
  for (let d = -3n; d <= 3n; d++) add(fromBits(b + d));
}
// Random bit patterns (finite only), fixed seed.
let s = 0x9e3779b97f4a7c15n;
const mask = (1n << 64n) - 1n;
const next = () => { s ^= s >> 12n; s ^= (s << 25n) & mask; s ^= s >> 27n; return (s * 0x2545f4914f6cdd1dn) & mask; };
let added = 0;
while (added < 4000) { const x = fromBits(next()); if (Number.isFinite(x)) { add(x); added++; } }

const seen = new Set();
const numbers = [];
for (const x of xs) {
  const b = bitsOf(x);
  if (seen.has(b)) continue;
  seen.add(b);
  numbers.push([b, String(x)]);
}
process.stdout.write(JSON.stringify({ generated_by: `node ${process.version} String(x)`, numbers }) + "\n");
