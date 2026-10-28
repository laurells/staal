# Maintenance: the same change made to both agents, as a developer might make it
# months later, and what then stops it. Each mutation is a list of exact
# replacements per agent; mutate.py applies them to a copy and runs every scenario.
#
# The first six are changes where the two languages differ. The last three are
# controls, where the change is to the policy or the business order itself, and
# neither language can tell it is wrong.

MUTATIONS = [
    {"name": "put the ticket into the model's instruction, for more context",
     "python": [("app.py", '"system": SYSTEM,', '"system": SYSTEM + "\\n\\nThe customer wrote: " + ticket,')],
     "staal": [("agent.staal", "instruct: POLICY, data: [ticket, order]",
                'instruct: POLICY + " The customer wrote: " + ticket, data: [ticket, order]')]},

    {"name": "drop validation of the plan: the model already answers in the schema",
     "python": [("app.py", "    return validate_plan(plan)", "    return plan")],
     "staal": [("agent.staal", "let plan: Plan = validate propose(", "let plan: Plan = propose(")]},

    {"name": "ask the model a second time to polish the email",
     "python": [("app.py",
                 '        if s["kind"] == "Email":\n'
                 '            send_email(journal, base, policy, s["to"], s["subject"], s["body"])',
                 '        if s["kind"] == "Email":\n'
                 '            req2, perform2 = ask_model(base, "Rewrite this email politely: " + s["body"], data)\n'
                 '            polished = read_plan(journal.call("model", req2, perform2, True))\n'
                 '            send_email(journal, base, policy, s["to"], s["subject"], polished["reason"])')],
     "staal": [("agent.staal",
                "                let m = replies.send(to, subject, body)",
                '                let polished: Plan = validate propose(Plan, instruct: "Rewrite the email in the data politely, as the reason.", data: [body], tokens: 800)\n'
                "                let m = replies.send(to, subject, polished.reason)")]},

    {"name": "refactor: each step goes through its own helper",
     "python": [("app.py",
                 '    policy = Policy(order)\n    refunded = 0\n    for s in plan["steps"]:\n'
                 '        if s["kind"] == "Refund":\n            refund(journal, base, policy,',
                 '    refunded = 0\n    for s in plan["steps"]:\n'
                 '        if s["kind"] == "Refund":\n            refund(journal, base, Policy(order),'),
                ("app.py", 'send_email(journal, base, policy, s["to"]', 'send_email(journal, base, Policy(order), s["to"]')],
     "staal": [("agent.staal",
                "                let amount = 0.01 USD * cents\n"
                "                let r = refunds.create(payment_intent, amount)\n",
                "                let amount = pay(payment_intent, cents, refunds)\n"),
               ("agent.staal", "                let m = replies.send(to, subject, body)",
                "                let m = send_reply(to, subject, body, replies)"),
               ("agent.staal", '    return "refunded " + to_text(refunded) + ", emails " + to_text(emailed)\n}\n',
                '    return "refunded " + to_text(refunded) + ", emails " + to_text(emailed)\n}\n\n'
                "fn pay(payment_intent: Text, cents: Int, refunds: cap stripe.refunds.create(uses: 1, each: 500 USD)) -> Money {\n"
                "    let amount = 0.01 USD * cents\n"
                "    let r = refunds.create(payment_intent, amount)\n"
                "    return amount\n}\n\n"
                "fn send_reply(to: Text, subject: Text, body: Text, replies: cap mail.messages.send(uses: 1)) -> Receipt {\n"
                "    return replies.send(to, subject, body)\n}\n")]},

    {"name": "send the email again if the mail service fails",
     "python": [("app.py",
                 '        try:\n'
                 '            status, text = request(base, "POST", "/emails", json.dumps(req), headers)\n'
                 '        except ConnectionRefusedError as e:\n'
                 '            raise Failed("action", "the mail service cannot be reached: " + str(e))\n'
                 '        return {"status": status, "body": text}   # Lost: the mail service does not deduplicate',
                 '        for attempt in range(2):\n'
                 '            try:\n'
                 '                status, text = request(base, "POST", "/emails", json.dumps(req), headers)\n'
                 '            except (ConnectionRefusedError, Lost):\n'
                 '                continue\n'
                 '            if status < 500:\n'
                 '                return {"status": status, "body": text}\n'
                 '        raise Failed("action", "the mail service did not take the message")')],
     "staal": [("agent.staal", "                let m = replies.send(to, subject, body)",
                "                let m = try replies.send(to, subject, body)\n"
                "                if not m.ok {\n"
                "                    let again = replies.send(to, subject, body)\n"
                "                }")]},

    {"name": "log every request sent, headers included, for debugging",
     "python": [("app.py", "            conn.request(method, path, body=body, headers=headers or {})",
                 '            with open("debug.log", "a") as log:\n'
                 "                log.write(json.dumps([method, path, headers, body]) + \"\\n\")\n"
                 "            conn.request(method, path, body=body, headers=headers or {})")],
     "staal": [("mail.staal", '        let r = http.post(base + "/emails", json(m),',
                '        let auth = header("Authorization", fill_secret("Bearer {secret}", key))\n'
                "        print(json([auth]))\n"
                '        let r = http.post(base + "/emails", json(m),')]},

    # Controls.
    {"name": "control: copy the support team on every reply", "control": True,
     "python": [("app.py", '            emailed += 1',
                 '            send_email(journal, base, policy, "support@shop.example", s["subject"], s["body"])\n'
                 '            emailed += 1')],
     "staal": [("agent.staal", "                set emailed = emailed + 1",
                '                let cc = replies.send("support@shop.example", subject, body)\n'
                "                set emailed = emailed + 1")]},

    {"name": "control: reply before refunding, so the customer hears sooner", "control": True,
     "python": [("app.py",
                 '    refunded = 0\n    for s in plan["steps"]:\n        if s["kind"] == "Refund":\n'
                 '            refund(journal, base, policy, s["payment_intent"], s["cents"])\n'
                 '            refunded += s["cents"]\n',
                 ""),
                ("app.py", '    return "refunded %d.%02d USD',
                 '    refunded = 0\n    for s in plan["steps"]:\n        if s["kind"] == "Refund":\n'
                 '            refund(journal, base, policy, s["payment_intent"], s["cents"])\n'
                 '            refunded += s["cents"]\n'
                 '    return "refunded %d.%02d USD')],
     "staal": [("agent.staal",
                "    let refunded = 0 USD\n    for s in plan.steps {\n        match s {\n            Refund {",
                "    let refunded = 0 USD\n    let emailed = 0\n    for s in plan.steps {\n        match s {\n"
                "            Email { to, subject, body } => {\n"
                "                let m = replies.send(to, subject, body)\n"
                "                set emailed = emailed + 1\n"
                "            }\n            _ => {}\n        }\n    }\n"
                "    for s in plan.steps {\n        match s {\n            Refund {"),
               ("agent.staal",
                "    let emailed = 0\n    for s in plan.steps {\n        match s {\n"
                "            Email { to, subject, body } => {\n"
                "                let m = replies.send(to, subject, body)\n"
                "                set emailed = emailed + 1\n"
                "            }\n            _ => {}\n        }\n    }\n    return",
                "    return")]},

    {"name": "control: raise the standard tier's limit to 500 USD", "control": True,
     "python": [("app.py", '"standard": 5000', '"standard": 50000')],
     "staal": [("app.py", '"standard": 5000', '"standard": 50000')]},
]
