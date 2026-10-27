// npm install @staal/node, then: node app.js
const path = require('path');
const staal = require('@staal/node');

const PROGRAM = path.join(__dirname, 'refund.staal');
const LIMIT = { gold: 500, standard: 50 }; // USD per refund, decided by your app

function refund(tier, order, amount) {
  return staal.run(PROGRAM, {
    entry: 'main',
    inputs: { order, amount: { amount, currency: 'USD' } },
    grants: { pay: { cap: 'payments.refund', each: { amount: String(LIMIT[tier]), currency: 'USD' } } },
  });
}

(async () => {
  console.log((await refund('standard', '4417', '35.00')).result);
  console.log((await refund('standard', '4417', '300.00')).failure.message);
})();
