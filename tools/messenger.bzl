"""messenger_generated: the messenger codegen as a build step.

A genrule runs basable-messenger-gen over routing.yaml and emits one
generated crate's src/lib.rs: `interfaces` (per nanoservice a router-generic
handler trait and a sender exposing exactly the declared sends) or
`messenger` (the concrete router routing every declared pair, boxed
back-edges on cycles). A sent message with no handler, two handlers for one
request or a response mismatch is a BUILD error naming the YAML line; the
routing.yaml is printed on failure so the log shows what the generator saw.

A tenant project loads this file from its own tools/ (the scaffolder renders
a copy) with the tool at its crate_universe label; this repository uses it
with the in-tree binary for the compile tests under tests/messenger.
"""

def messenger_generated(
        name,
        crate,
        spec = "//:routing.yaml",
        tool = "@crates//:basable-messenger-gen__basable-messenger-gen",
        out = "src/lib.rs"):
    """Generates a crate's src/lib.rs from routing.yaml.

    Args:
        name: the genrule name.
        crate: "interfaces" or "messenger".
        spec: the routing.yaml label.
        tool: the basable-messenger-gen binary label.
        out: the output path (default src/lib.rs).
    """
    native.genrule(
        name = name,
        srcs = [spec],
        outs = [out],
        cmd = "$(location {tool}) generate --crate {crate} --spec $(SRCS) --output $@ || (echo '--- routing.yaml ---'; cat $(SRCS); exit 1)".format(
            tool = tool,
            crate = crate,
        ),
        tools = [tool],
    )
