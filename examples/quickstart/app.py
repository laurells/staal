# pip install staal, then: python3 app.py
import os

import staal

lib = staal.Library()
PROGRAM = os.path.join(os.path.dirname(os.path.abspath(__file__)), "refund.staal")
LIMIT = {"gold": 500, "standard": 50}  # USD per refund, decided by your app


def refund(tier, order, amount):
    return lib.run(PROGRAM, {
        "entry": "main",
        "inputs": {"order": order, "amount": {"amount": amount, "currency": "USD"}},
        "grants": {"pay": {"cap": "payments.refund", "each": {"amount": str(LIMIT[tier]), "currency": "USD"}}},
    })


print(refund("standard", "4417", "35.00")["result"])
print(refund("standard", "4417", "300.00")["failure"]["message"])
