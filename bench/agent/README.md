# Agent benchmark: Python alone, and Python with Staal

The same support agent is built twice. A model reads a customer's ticket and
proposes a plan of at most one refund and at most one email. The app executes the
plan against Stripe and a mail service. One version is Python alone. The other is
Python with Staal: the plan runs in Staal and the app grants it its limits. Both
versions use the same policy, and both are run against the same simulated world.

## The specification both versions meet

A standard customer may be refunded up to 50 USD, a gold customer up to 500 USD,
and never more than the order cost. A job may refund only the order's own payment,
in the payment's currency, at most once. It may email only the order's customer,
at most once. Refunds run before the email, and a refused or failed refund means no
email is sent.

A job makes one model call of at most 800 tokens. An overloaded model is retried
twice, after which the job stops and can be resumed later. If Stripe's answer is
lost, the refund is sent again with the same idempotency key, because Stripe
deduplicates by that key. The mail service doesn't deduplicate, so an email whose
answer was lost is never sent again: the job stops, and an operator checks the mail
service and records what happened.

A job that is killed at any point is started again and must finish as an
uninterrupted job would. If the model is asked again after a crash, its second
answer differs from its first. The ticket is never placed in the model's
instruction, and no secret is written to any file.

## The world

`world.py` puts three services on one local port. The first is a model that answers
like Anthropic's Messages API with whatever plan the scenario scripts. It is a
gullible model: when the ticket carries an injection, the script has it obey. The
second is a Stripe that checks the key, the payment, the amount and the idempotency
key the way Stripe documents. The third is a mail service with no deduplication.

The world records every request and every effect. It can kill the agent's process
at a chosen request, either before the request takes effect or after it took effect
but before the answer is sent. Both versions are therefore killed at exactly the
same points. A scenario passes only when the effects the world ended up with are
ones the scenario allows, and no invariant broke.

## Results

Run on 2026-10-28 on Linux x86_64. The Staal version used staalc built from this
commit, which includes the fix described below.

**Failure cases prevented.** There are 47 scenarios:

- 6 on policy
- 4 on prompt injection
- 12 on malformed model output
- 7 on permission escalation
- 8 on failing services and lost answers
- 10 on crashes around every external call

Both versions pass all 47. Both stopped for an operator in the same 4 scenarios
(an email whose answer was lost), and both were killed and restarted 11 times.

Staal 0.24.0, the release before this fix, fails 2 of the 47: "Stripe's answer is lost"
and "crash after the refund, with Stripe's answer lost on the retry". When a keyed
POST lost its answer, 0.24.0 told the program the action had failed. The refund was
made, the job reported failure, and the email was never sent. A program that caught
that failure and retried would have sent the refund again under a new key, which
Stripe would not deduplicate. Now the run stops, and resuming sends the request
again with the same key (see the changelog). The hand-written Python handled this
case from the start.

One injection gets through both versions, and it should: a ticket that talks the
model into a 49 USD refund on an order that was fine. The refund is within the
customer's limit, on their own payment. Neither version can tell a lie that stays
inside the policy from the truth. Only the policy limits the damage.

**Amount of code.** This counts lines that are not blank and not comments
(`loc.py`):

| | Lines |
| --- | --- |
| Python alone: `app.py` | 312 |
| Python with Staal: the agent (`agent.staal`) | 39 |
| Python with Staal: the app (`app.py`): policy as grants, run, resume, resolve | 59 |
| Python with Staal: Stripe library | 39 |
| Python with Staal: mail library | 22 |

The Python-alone total is 312 lines. The Python-with-Staal total is 159 lines, or 98
if the two libraries come as packages. The Stripe library already ships in
`examples/capabilities` and, adapted, in `packages/payments`.

In the Python version, 78 lines are the journal, 20 classify whether a request was
sent, 60 call and check the model, and 82 are the policy and the actions.

**Maintenance.** `mutations.py` makes the same change to both versions, the way a
developer might months later. `mutate.py` then checks each changed version the way
its language allows and runs all 47 scenarios against it:

| Change | Python alone | Python with Staal |
| --- | --- | --- |
| Put the ticket into the model's instruction | ships; 47 scenarios break the rule | refused before it runs (E006) |
| Drop validation of the plan | ships; 4 scenarios act on malformed plans, 1 crashes the job | refused before it runs (E001) |
| Ask the model again to polish the email | ships; 24 scenarios go over the budget | refused before it runs (E004) |
| Refactor each step into its own helper | ships; two refunds of 40 USD go through where one is allowed | runs; the limits still hold |
| Send the email again if the mail service fails | ships; a customer is emailed twice when an answer is lost, and an email the spec says to drop goes out after a 500 | runs; the limits still hold |
| Log every request, headers included | ships; the Stripe and mail keys land in a log | refused before it runs (E101) |
| Control: copy support on every reply | runs; the policy refuses it | runs; the policy refuses it |
| Control: reply before refunding | ships; 12 scenarios break | ships; 12 scenarios break |
| Control: raise the standard limit to 500 USD | ships; 2 scenarios break | ships; 2 scenarios break |

Of the six changes where the languages differ, all six ship broken in the Python
version unless its own tests happen to cover them. In the Staal version, four are
refused before anything runs and two run with every guarantee intact.

The three controls are changes to the policy or to the order of business, and
neither language can tell they are wrong. The controls are there to show where the
line is. Staal stops changes that break how an action is authorised, checked or
recorded. It does not stop changes to what the business decided.

**Operational work.**

- *Python alone:* the team owns its journal, including the format, replay and
  divergence checks, torn writes and the single-runner lock. It also owns the
  decision of which failures may have reached a service, the idempotency key scheme,
  the in-doubt rules for each service, the resolve command, the retry loops, the
  plan validator and its schema, and the policy object. All of it is the team's to
  test against crashes, and to keep right whenever the agent changes.
- *Python with Staal:* the team owns the grants and a short run, resume and
  resolve dispatch.
- *Operators:* the work is the same for both, one resolution for each email in
  doubt.
- *What Staal adds:* a journal is tied to the program, the config and the runtime
  version. A job still in flight must finish before staalc is upgraded or the agent
  is changed, or resuming refuses. The Python version checks only that the job
  itself is unchanged and that each operation matches.

## What this does not show

The Python version was written by the same person, against a specification and
scenarios they knew in advance. That favours Python. A team's own Python agent,
written without the scenario list, is likely to be weaker, and this benchmark does
not measure how much.

The model is scripted, Stripe and the mail service are local simulations, and
crashes are `SIGKILL` at request boundaries. Power loss is not tested, and neither
is a crash between receiving an answer and recording it. Staal's own test suite
injects crashes at every journal write; the Python version has no such tests.

Line counts measure size, not difficulty.

## Running it

```
pip install staal        # 0.24.1 or later for all 47
python run.py            # every scenario against both agents
python mutate.py         # the maintenance study (about 10 minutes)
python loc.py            # lines of code
```

`results/` holds the results above as JSON.
