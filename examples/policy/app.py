"""Your app decides the policy; Staal enforces it.

The limit on each refund depends on things only the app knows: the customer's tier
and what the order cost. The app works it out and grants it to the run. Staal then
refuses any refund above it, whatever the model proposes, and never lets a refund past
the 500 USD the code itself allows, whatever the app grants.

    STAAL_LIBRARY=/path/to/libstaalc.so ANTHROPIC_API_KEY=... python3 app.py

(With the staal package installed from a wheel, STAAL_LIBRARY is not needed.)
"""

import os

import staal

PROGRAM = os.path.join(os.path.dirname(os.path.abspath(__file__)), "refund.staal")

# USD per refund, by tier.
TIER_LIMIT = {"gold": 500, "standard": 50}

CLAUDE = {"provider": "anthropic", "model": "claude-sonnet-5-5", "api_key": {"env": "ANTHROPIC_API_KEY"}}


def refund_limit(customer, order_total):
    """The most one refund may be, in whole USD: the tier's limit, and never more than the order cost."""
    return min(TIER_LIMIT[customer["tier"]], order_total)


def handle(lib, customer, order_total, complaint, model=CLAUDE, journal=None, engine=None):
    """Runs the agent for one complaint, with this customer's limit granted."""
    limit = refund_limit(customer, order_total)
    config = {
        "entry": "main",
        "inputs": {"complaint": complaint},
        "grants": {"pay": {"cap": "payments.refund", "each": {"amount": str(limit), "currency": "USD"}}},
        "model": model,
    }
    return lib.run(PROGRAM, config, journal=journal, engine=engine)


if __name__ == "__main__":
    lib = staal.Library()
    customer = {"id": "c-102", "tier": "standard"}
    out = handle(lib, customer, 80, "My blender arrived broken two days ago. Order 4417.")
    if out["status"] == "done":
        print(out["result"])
    elif out["status"] == "failed":
        print("refused:", out["failure"]["message"])
    else:
        print(out["status"], out.get("error") or out.get("errors"))
