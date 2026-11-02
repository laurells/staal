# Write your first Staal package

A package turns one service into a library capability: programs ask for it with
limits, the operator grants it with keys, and the runtime holds every action to both.
This guide takes one real service, Postmark's email API, from an empty folder to a
package with tests that pass, ready for a pull request. It takes about an hour, and
needs no knowledge of the compiler.

The guide is tested: `tests/package_cases_test.rs` saves every file below into an
empty folder and runs its tests, so what you read here works as written.

You need a clone of the repository, Rust (`cargo`), and git:

    git clone https://github.com/laurells/staal.git
    cd staal
    cargo build

## 1. Read the service's documentation first

Everything a package does comes from the service's own documentation, never from
memory. For Postmark's [single email](https://postmarkapp.com/developer/user-guide/send-email-with-api/send-a-single-email):

- `POST https://api.postmarkapp.com/email`, with the headers `Accept:
  application/json`, `Content-Type: application/json` and `X-Postmark-Server-Token`.
- A JSON body with `From`, `To`, `Subject` and `TextBody`.
- A success answer with `ErrorCode: 0` and the email's `MessageID`; a 401 for a wrong
  token, a 422 for a sender that is not confirmed.
- Nothing about idempotency keys: Postmark does not deduplicate a request sent twice.

Write these down. Each becomes a line of the library or of a test.

## 2. Start from the template

Packages are grouped by kind (`email`, `sms`, `chat`). Copy the template into a new
folder:

    cp -r packages/template packages/email

(If the folder already exists, as `packages/email` does for Resend, add your library
file to it and its URL to its `staal.json` instead.) The template's `notes.staal` and
its tests show every part; you will replace them.

## 3. Declare the authority

`staal.json` names the package and every URL it may reach. The compiler refuses any
request the library makes outside it (E108), so a reviewer only has to read this file
to know what the package can touch:

```json file=staal.json
{
  "package": "email",
  "version": "0.1.0",
  "authority": {
    "net.http.post": ["https://api.postmarkapp.com/email"]
  }
}
```

## 4. Write the library

A library capability declares what the operator must supply (`needs`), how a target
is written (`canonical`), and its actions. Each action must make its request through
what it needs, and its `@target` must reach that request, so `only` limits the value
really used:

```staal file=postmark.staal
// Email through Postmark (https://postmarkapp.com).
//
// Programs ask for `cap email.postmark.send(only: ["*@clinic.org"], uses: N)` and call
// `mail.send(to, subject, text)`. The operator grants the HTTP capability, the server
// token and the sender address under `needs`. Postmark documents no idempotency key,
// so grant `http` without `idempotency_key`: an email whose answer a crash loses stops
// the run in doubt, for an operator to check in Postmark's activity, instead of being
// sent twice.
//
// `canonical` refuses addresses with capital letters rather than lowercasing them:
// the part before '@' may be case-sensitive, and two addresses must never be made one
// for the sake of an `only` check.

type Email { From: Text, To: Text, Subject: Text, TextBody: Text }
type Sent { MessageID: Text, ErrorCode: Int }

capability email.postmark {
    needs http: cap net.http.post(only: ["https://api.postmarkapp.com/email"])
    needs token: Secret
    needs sender: Text

    canonical(address: Text) -> Text {
        let a = trim(address)
        if lower(a) != a {
            fail("write the address in lower case: " + a)
        }
        if not contains(a, "@") {
            fail("'" + a + "' is not an email address")
        }
        return a
    }

    action send(to: Text @target, subject: Text, text: Text) -> Text {
        let email = Email { From: sender, To: trim(to), Subject: subject, TextBody: text }
        let r = http.post("https://api.postmarkapp.com/email", json(email),
            [header("Accept", "application/json"),
                header("Content-Type", "application/json"),
                header("X-Postmark-Server-Token", token)])
        if r.status != 200 or not r.complete {
            fail("Postmark answered " + json(r.status) + ": " + r.body)
        }
        let sent: Sent = validate parse(Sent, r.body)
        if sent.ErrorCode != 0 {
            fail("Postmark refused the email: " + r.body)
        }
        return sent.MessageID
    }
}
```

Three things to notice. The token is a `Secret`: it can go into a header, and nowhere
else, and the runtime keeps it out of every log and journal. The answer is untrusted
until `validate parse(...)` checks it against `Sent`, which keeps the fields named and
drops the rest. And every failure says what the service answered, so an operator can
act on it.

Delete the template's `notes.staal` now.

## 5. Describe the service's answers

A package is tested against a fake service: a local server that answers as the
documentation shows. You do not write a server; you list its routes in a test file,
`tests/<name>.json`, together with a program that uses the package, its grants, and
what must happen:

```json file=tests/send.json
{
  "about": "An email within the grant is sent once, as Postmark documents it, with the token in its header.",
  "service": "https://api.postmarkapp.com",
  "routes": [
    { "method": "POST", "path": "/email", "status": 200,
      "body": { "To": "ada@clinic.org", "SubmittedAt": "2026-11-02T10:00:00Z", "MessageID": "msg-{n}", "ErrorCode": 0, "Message": "OK" } }
  ],
  "program": "import email.postmark\n\nfn main(mail: cap email.postmark.send(uses: 1)) -> Receipt {\n    return mail.send(\"ada@clinic.org\", \"Results\", \"Your results are ready.\")\n}\n",
  "grants": {
    "mail": { "cap": "email.postmark.send", "only": ["*@clinic.org"], "needs": {
      "http": { "cap": "net.http.post", "only": ["{service}/email"] },
      "token": { "env": "STAAL_PACKAGE_KEY" },
      "sender": "results@clinic.org"
    } }
  },
  "expect": {
    "result": { "receipt": "msg-1", "capability": "email.postmark.send", "method": "send" },
    "requests": [
      { "method": "POST", "path": "/email",
        "json": { "From": "results@clinic.org", "To": "ada@clinic.org", "Subject": "Results", "TextBody": "Your results are ready." },
        "headers": { "X-Postmark-Server-Token": "{key}", "Accept": "application/json" } }
    ]
  }
}
```

`service` is the base URL the package reaches; the test replaces it with the fake's
address, in the package's code and manifest alike, and `{service}` in the grants
stands for it. Every secret is granted as `{"env": "STAAL_PACKAGE_KEY"}`, and `{key}`
in an expected header stands for its value. `{n}` in an answer is the request's
number, so two emails that were really sent are told apart. `requests` lists every
request the service received, in order.

Every package needs a test that a refused action never reaches the service:

```json file=tests/refused.json
{
  "about": "An email to an address outside `only` is refused before any request is made.",
  "service": "https://api.postmarkapp.com",
  "routes": [
    { "method": "POST", "path": "/email", "status": 200, "body": { "MessageID": "msg-{n}", "ErrorCode": 0 } }
  ],
  "program": "import email.postmark\n\nfn main(mail: cap email.postmark.send) -> Receipt {\n    return mail.send(\"eve@elsewhere.com\", \"Results\", \"Hello.\")\n}\n",
  "grants": {
    "mail": { "cap": "email.postmark.send", "only": ["*@clinic.org"], "needs": {
      "http": { "cap": "net.http.post", "only": ["{service}/email"] },
      "token": { "env": "STAAL_PACKAGE_KEY" },
      "sender": "results@clinic.org"
    } }
  },
  "expect": {
    "failure": { "kind": "capability", "message": "this action targets \"eve@elsewhere.com\"" },
    "requests": []
  }
}
```

And one for each way the service says no, with the answer the documentation shows:

```json file=tests/unconfirmed-sender.json
{
  "about": "Postmark's 422 for an unconfirmed sender fails the action with Postmark's answer, and nothing is retried.",
  "service": "https://api.postmarkapp.com",
  "routes": [
    { "method": "POST", "path": "/email", "status": 422,
      "body": { "ErrorCode": 400, "Message": "The 'From' address you supplied is not a Sender Signature on your account." } }
  ],
  "program": "import email.postmark\n\nfn main(mail: cap email.postmark.send) -> Receipt {\n    return mail.send(\"ada@clinic.org\", \"Results\", \"Hello.\")\n}\n",
  "grants": {
    "mail": { "cap": "email.postmark.send", "only": ["*@clinic.org"], "needs": {
      "http": { "cap": "net.http.post", "only": ["{service}/email"] },
      "token": { "env": "STAAL_PACKAGE_KEY" },
      "sender": "results@clinic.org"
    } }
  },
  "expect": {
    "failure": { "kind": "action", "message": "Postmark answered 422" },
    "requests": [ { "method": "POST", "path": "/email" } ]
  }
}
```

Delete the template's tests.

## 6. Run the tests

    STAAL_PACKAGE=email cargo test --test package_cases_test

Each test runs the program against the fake, checks the result and every request,
and checks the token appears in neither the run's log nor its journal. Then run the
crash tests:

    STAAL_PACKAGE=email cargo test --features failpoints --test package_cases_test

Each test is run again, killed after every journal write and between every request
and its recorded answer, then resumed; an email left in doubt is resolved from what
the fake really received. Every run must end as the uninterrupted one did, with the
same emails sent: none lost, none twice. You wrote none of this; it comes with every
package.

When a test fails, it names the test file and says what differed: a request's body,
a header, the result, or what the service saw after a crash.

## 7. Write the README section, and open the pull request

Add your capability to the table in `packages/README.md`, with a grant example:

    "mail": { "cap": "email.postmark.send", "only": ["*@clinic.org"],
              "needs": { "http": { "cap": "net.http.post", "only": ["https://api.postmarkapp.com/email"] },
                         "token": { "env": "POSTMARK_SERVER_TOKEN" },
                         "sender": "results@clinic.org" } }

and an honest status: "tested against a local server only", until someone runs it
against a real account with a test token.

Then check your work against the list in `packages/README.md` ("What every package
must meet"), run `cargo test`, and open a pull request. The first response comes
within two working days (CONTRIBUTING.md).
