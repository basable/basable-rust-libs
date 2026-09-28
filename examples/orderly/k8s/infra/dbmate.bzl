"""dbmate_image(): the migration image, vendored from the basable platform's
k8s/infra/dbmate.bzl (single platform)."""

load("@rules_multirun//:defs.bzl", "multirun")
load("@rules_oci//oci:defs.bzl", "oci_image", "oci_push")
load("@rules_pkg//pkg:tar.bzl", "pkg_tar")
load("//k8s/infra:image.bzl", "build_sha265_tag")

def dbmate_image(name, migrations, package_dir = "/migrations", repositories = [], visibility = None):
    """dbmate plus the migrations, pushed as <repository>-<name>."""
    pkg_tar(
        name = "{}_migrations_tar".format(name),
        srcs = migrations,
        package_dir = package_dir,
    )
    oci_image(
        name = "{}_image".format(name),
        base = "@dbmate_linux_amd64",
        tars = [":{}_migrations_tar".format(name)],
        workdir = "/",
        visibility = ["//visibility:public"],
    )
    build_sha265_tag(
        name = "{}_remote_tag".format(name),
        image = ":{}_image".format(name),
        input = ":{}_image.json.sha256".format(name),
        output = "{}_tag.txt".format(name),
    )
    pushes = []
    for repo in repositories:
        repo_safe = repo.replace(":", "_").replace("/", "_").replace(".", "_")
        oci_push(
            name = "{}_{}_push".format(name, repo_safe),
            image = ":{}_image".format(name),
            remote_tags = ":{}_remote_tag".format(name),
            repository = "{}-{}".format(repo, name),
        )
        pushes.append(":{}_{}_push".format(name, repo_safe))
    multirun(
        name = "{}_push".format(name),
        commands = pushes,
        visibility = visibility or ["//visibility:public"],
    )
