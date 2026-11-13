"use strict";
Object.defineProperty(exports, "__esModule", { value: true });
function fib(n) {
    if (n < 2)
        return n;
    return fib(n - 1) + fib(n - 2);
}
console.log(fib(Number(process.argv[2])));
