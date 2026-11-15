# Concurrency

`parallel(list, f, ...)` calls `f(item, ...)` for every item of a list at once, and
gives the results in the list's order.

```staal file=digest.staal
type Note { text: Text }

fn summarize(topic: Text) -> Text budget(calls: 1, tokens: 400) {
    let n: Note = validate propose(Note, instruct: "Summarize the topic in one sentence.", data: [topic], tokens: 400)
    return n.text
}

fn notify(to: Text, mail: cap email.send) -> Receipt {
    return mail.send(to, "The digest is ready.")
}

fn digest(topics: List<Text, 3>, people: List<Text, 3>, mail: cap email.send) -> List<Text, 3> budget(calls: 3, tokens: 1200) {
    let sent = parallel(people, notify, mail)
    return parallel(topics, summarize)
}
```

```json file=digest.json
{
  "entry": "digest",
  "inputs": { "topics": ["refunds", "shipping"], "people": ["ada@example.com", "bo@example.com"] },
  "grants": { "mail": { "cap": "email.send" } },
  "model": { "provider": "script", "responses": [
    { "output": { "text": "Refunds are issued within five days." }, "when": "refunds" },
    { "output": { "text": "Orders ship within two days." }, "when": "shipping" }
  ] }
}
```

```console
$ staalc run digest.staal --config digest.json
["Refunds are issued within five days.","Orders ship within two days."]
```

- `f` is a declared function's name (`m.f` from a module), not a function value. Its
  first parameter takes the item. The values after `f` are names or literals,
  evaluated once and given to every task.
- A task can call a model and perform actions through the capabilities it is given.
- Tasks run on up to 16 threads at a time; the rest wait for a thread.
- Every task runs to its end. If any failed, `parallel` fails with the first failure
  by position in the list, except that a failure which stops the run (`in_doubt`,
  `journal`) comes before any other. The outcome does not depend on scheduling.
- Tasks share no data: values cannot be changed once made, and `set` changes only a
  function's own variables.
- A budget counts the call once per item the list may hold (E004): `digest` declares
  three calls for a list of at most three topics.
- The log lists each task's events in the list's order, each with `"task"`.

## Limited capabilities

A capability limited by `uses` or `max`, in its grant, anywhere in its chain, or in a
library capability's `needs`, cannot be passed to tasks: which task would be refused
would depend on their timing. `each` and `only` are checked action by action and can.

```staal file=limited.staal
fn notify(to: Text, mail: cap email.send(uses: 2)) -> Receipt {
    return mail.send(to, "Hello.")
}

fn notify_everyone(people: List<Text, 3>, mail: cap email.send(uses: 2)) -> Int {
    let sent = parallel(people, notify, mail)
    return len(sent)
}
```

```json file=limited.json
{ "entry": "notify_everyone", "inputs": { "people": ["a@x.org"] }, "grants": { "mail": { "cap": "email.send" } } }
```

```console
$ staalc run limited.staal --config limited.json
staalc: run failed: capability: 'email.send' is limited by `uses` or `max`, so it cannot be shared by concurrent tasks: which of them would be refused would depend on their timing. Use a `for` loop
```

The program checks; the run fails with kind `capability` and exit code 1.

## Crashes

Each task's operations are a separate stream in the journal, named by the task's id:
the number of the `parallel` call in its function's run and the item's position
(`2.0`, `2.1`; a task's own tasks `2.1/1.0`). A resumed run gives every task the same
id, so each replays what it recorded, whatever order the tasks run in.

A crash can leave one action in doubt in each task that was in flight. `staalc resolve`
names the one it settles with `--operation <id>`: `2.1:1` is operation 1 of task 2.1.
See [Journals and resuming](durability.md) and [RUNTIME.md](../RUNTIME.md), section 18.
