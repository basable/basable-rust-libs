# Declarative configuration seed

One JSON file per config type declared in the plan (`owns.configTypes`),
plus `namespaces.json` declaring the namespaces the objects are filed under.
Each file is `{"configSetName": …, "items": [...]}`; each item is one
configuration message in protobuf JSON with an `@type` naming the message
(`<nanoservice>.v1.<Name>Configuration`) and a `header` giving the object's
`namespace` (a `#{NamespaceConfiguration:<name>}` reference, never a bare
name), its `name` and its `labels`. A field that references another object
holds `#{Type:namespace:name}` and arrives in the app as that object's id.

`basable-config`'s loader applies the directory at boot in one transaction,
dependencies first, and prunes every object it manages that the files no
longer declare: removing an item is the delete. A re-run that changes
nothing writes nothing; a change is a closed version in the history table.
Objects the app writes at runtime are never touched. Every dependency must
be declared in this directory: an object's namespace and every object it
references. A file named `<name>.<env>.json` applies only in that
environment (`dev`, `prod`, `test`; several tokens allowed); a bare
`<name>.json` applies everywhere.

Objects here are edited by people and read by the app. Anything the system
creates and converges is a processing object, not a catalog entry.
