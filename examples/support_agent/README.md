# A support agent, ready to publish

A customer writes about one of their orders. Claude reads the message and chooses
Refund, Decline or Escalate; the program holds that choice to the business's rules
before any money moves, and refunds through the business's own payment service.

| File | What it is |
| --- | --- |
| `support.staal` | the agent: its rules, its payment capability, and `handle`, the tool |
| `support_test.staal` | the rules tested on their own, with no model and no money |
| `agent.json` | the publisher's description: what the agent is for, and its data handling |
| `agent.described.json` | what `staalc` worked out the program can do, written by `staalc agent describe` |
| `run.json` | a run with Claude and a payment service of your own |

What the compiler proves before it runs: the customer's message reaches Claude as data
and never as an instruction; Claude's answer is used only after it is checked to be one
of the three declared choices; the agent can make one refund of at most 100 USD and
nothing else; and it makes at most one model call of 1500 tokens. At run time, every
refund is checked again against those limits, and against the one address the business
granted for its payment service, before it is sent.

    staalc check support.staal
    staalc test support_test.staal
    staalc agent check .

A business installing it supplies the customer and the order from its own records
(`app_inputs` in agent.json: the agent's guarantees hold only if they are really the
signed-in customer's), the host of its payment service for `https://{{PAYMENTS}}/refunds`,
and its key. To run it, change the address in `run.json` to your payment service, then:

    export ANTHROPIC_API_KEY=... PAYMENTS_KEY=...
    staalc run support.staal --config run.json

`tests/support_agent_test.rs` runs it against a local server that answers like a payment
API, with a scripted model in Claude's place: a fair refund is sent once, with the
business's key; a refund above what the customer paid, an answer outside the three
choices, and a decline send nothing; and a payment service granted at another address
is never reached.

What it does not guarantee: that Claude judges each complaint well, only that a wrong
judgement stays within the rules. The data handling section of agent.json is the
publisher's statement, which Staal cannot check.
