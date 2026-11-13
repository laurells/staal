// Exact money as integer minor units (cents) in a number, exact below 2^53, as
// TypeScript code commonly keeps it.
const n = Number(process.argv[2]);
const price = 37;
let total = 0;
for (let i = 0; i < n; i++) {
  total += price * ((i % 10) + 1);
}
console.log(`"${Math.trunc(total / 100)}.${String(total % 100).padStart(2, "0")} USD"`);

export {};
