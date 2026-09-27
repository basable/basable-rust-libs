"""messenger_crates: the messenger codegen as a build step.

A genrule runs the basable-messenger-gen binary (crate_universe
`gen_binaries`) over //:routing.yaml and emits the two generated crates'
sources: `interfaces` (per nanoservice a router-generic handler trait and a
sender exposing exactly the declared sends) and `messenger` (the concrete
AppMessenger routing every declared pair, boxed back-edges on cycles). A sent
message with no handler, two handlers for one request or a response
mismatch is a BUILD error naming the YAML line; the generated file is printed
on failure so the log shows exactly what the traits expect.
"""

def messenger_generated(name, crate):
    """Generates crates/<crate>/src/lib.rs from //:routing.yaml.

    Args:
        name: the genrule name.
        crate: "interfaces" or "messenger".
    """
    native.genrule(
        name = name,
        srcs = ["//:routing.yaml"],
        outs = ["src/lib.rs"],
        cmd = "$(location @crates//:basable-messenger-gen__basable-messenger-gen) generate --crate " + crate + " --spec $(SRCS) --output $@ || (echo '--- routing.yaml ---'; cat $(SRCS); exit 1)",
        tools = ["@crates//:basable-messenger-gen__basable-messenger-gen"],
    )
