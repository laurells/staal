"use strict";
Object.defineProperty(exports, "__esModule", { value: true });
const n = Number(process.argv[2]);
let s = 0;
for (let i = 0; i < n; i++) {
    s += (i * i) % 7;
}
console.log(s);
