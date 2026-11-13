const n = Number(process.argv[2]);
const t = "abcdéfghij".repeat(Math.floor(n / 10));
let s = 0;
for (let j = 0; j < 1000; j++) {
  // Characters, not UTF-16 units: the same count as the other languages.
  s += [...t].length;
  if (t.includes(String(j))) s += 1;
}
console.log(s);

export {};
