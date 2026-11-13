const n = Number(process.argv[2]);
let t = "";
for (let i = 0; i < n; i++) {
  t += String(i) + ",";
}
console.log(t.length);

export {};
