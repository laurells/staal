# Generates large multi-module Intent programs for timing `intentc check`.
#
#   python3 bench/gen_program.py OUT_DIR N [N ...]
#   cargo build --release && time ./target/release/intentc check OUT_DIR/pN/main.int
#
# Each program has N functions spread over 20 modules, plus a module of shared
# types. Every function has a bounded loop with a branch, two reassigned
# variables, Text and Int arithmetic, one or two calls into lower modules, and
# about one in ten makes a model call. The programs are valid: checking succeeds.
import os, random, sys, shutil
def make(root, n_fns, n_mods=20):
    shutil.rmtree(root, ignore_errors=True); os.makedirs(root)
    random.seed(7)
    per = max(1, n_fns // n_mods)
    open(f"{root}/types.int","w").write("pub type Customer { name: Text, email: Text }\npub type Item { sku: Text, qty: Int }\n")
    for m in range(n_mods):
        out = ["import types { Customer, Item }"]
        if m > 0:
            out.append(f"import m{m-1}")
            if m > 1: out.append(f"import m{m-2}")
        out.append("")
        for i in range(per):
            calls = []
            if m > 0: calls.append(f"m{m-1}.f{random.randrange(per)}(items, name)")
            if m > 1 and random.random() < 0.5: calls.append(f"m{m-2}.f{random.randrange(per)}(items, name)")
            model = random.random() < 0.1
            body = [
                f"pub fn f{i}(items: List<Item, 20>, name: Text) -> Int {{",
                "    let n = 0",
                "    let label = name",
                "    for it in items {",
                "        if it.qty > 2 and starts_with(it.sku, \"A\") {",
                "            set n = n + it.qty",
                "        } else {",
                "            set label = label + it.sku",
                "        }",
                "    }",
            ]
            for k, cexpr in enumerate(calls):
                body.append(f"    let a{k} = {cexpr}")
                body.append(f"    set n = n + a{k}")
            if model:
                body.append("    let s = validate propose(Text, instruct: \"Summarise the label in the data.\", data: [label], tokens: 50)")
            body.append("    return n")
            body.append("}")
            out += body + [""]
        open(f"{root}/m{m}.int","w").write("\n".join(out))
    main = [f"import m{n_mods-1}", "import types { Customer, Item }", "",
            "fn main(items: List<Item, 20>, c: Customer @blind(email)) -> Int {",
            f"    return m{n_mods-1}.f0(items, c.name)", "}", ""]
    open(f"{root}/main.int","w").write("\n".join(main))
    return sum(len(open(os.path.join(root,f)).read().splitlines()) for f in os.listdir(root))
out_dir = sys.argv[1]
for n in map(int, sys.argv[2:]):
    lines = make(os.path.join(out_dir, f"p{n}"), n)
    print(n, lines)
