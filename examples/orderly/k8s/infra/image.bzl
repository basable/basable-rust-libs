"""image(): an OCI image from one binary and its push targets. Vendored from
the basable platform's k8s/infra/image.bzl, single-platform (x86_64 Linux).

`bazel run //<pkg>:<name>_push -- --tag <sha>` pushes the digest tag the
macro derives from the image plus the commit-SHA tag the platform contract
names; the workflow reads the digest back from bazel-bin/<pkg>/<name>_image/index.json.
"""

load("@rules_multirun//:defs.bzl", "multirun")
load("@rules_oci//oci:defs.bzl", "oci_image", "oci_push")
load("@rules_pkg//pkg:tar.bzl", "pkg_tar")

def _build_sha265_tag_impl(ctx):
    in_file = ctx.file.input
    out_file = ctx.outputs.output
    ctx.actions.run_shell(
        inputs = [in_file],
        outputs = [out_file],
        arguments = [in_file.path, out_file.path],
        command = "sed -n 's/.*sha256:\\([0-9a-f]\\{7\\}\\).*/\\1/p' < \"$1\" > \"$2\"",
    )

build_sha265_tag = rule(
    doc = "Extracts a 7 characters long short hash from the image digest.",
    implementation = _build_sha265_tag_impl,
    attrs = {
        "image": attr.label(allow_single_file = True, mandatory = True),
        "input": attr.label(allow_single_file = True, mandatory = True, doc = "The image digest file (image.json.sha256)."),
        "output": attr.output(doc = "The generated tag file."),
    },
)

def image(name, binary, base, exposed_ports = [], repositories = [], user = "65532", extra_tars = []):
    """Packages binary into a layer, builds the image on base, mints the push targets.

    Args:
        name: name of the image and related targets.
        binary: the binary label; it becomes /<name> inside the image.
        base: the base image repository label (single platform).
        exposed_ports: ports to expose.
        repositories: registries (with the image path) to push to.
        user: numeric user the container runs as.
        extra_tars: additional layers.
    """
    pkg_tar(
        name = "{}_tar".format(name),
        srcs = [binary],
        remap_paths = {binary.lstrip(":"): name},
    )
    oci_image(
        name = "{}_image".format(name),
        base = base,
        entrypoint = ["/{}".format(name)],
        exposed_ports = exposed_ports,
        tars = ["{}_tar".format(name)] + extra_tars,
        user = user,
        visibility = ["//visibility:public"],
    )
    build_sha265_tag(
        name = "{}_remote_tag".format(name),
        image = "{}_image".format(name),
        input = "{}_image.json.sha256".format(name),
        output = "{}_tag.txt".format(name),
    )
    pushes = []
    for repo in repositories:
        repo_name = repo.replace(":", "_").replace("/", "_").replace(".", "_")
        oci_push(
            name = "{}_{}_push".format(name, repo_name),
            image = "{}_image".format(name),
            remote_tags = "{}_remote_tag".format(name),
            repository = repo,
            visibility = ["//visibility:public"],
        )
        pushes.append("{}_{}_push".format(name, repo_name))
    multirun(
        name = "{}_push".format(name),
        commands = pushes,
        visibility = ["//visibility:public"],
    )
