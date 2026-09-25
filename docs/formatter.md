# Formatting Mux source

The formatter is available in compiler builds from this branch. It may not be
available in the latest published release.

Run `mux format` from a project directory to format its `.mux` files
recursively. You can also pass files or directories:

```sh
mux format
mux format src tests
mux format ~/test.mux
```

The formatter uses four-space indentation and an 80-column target. It keeps
literal spellings, comment contents, and declaration order. Long literals,
comments, and expressions without a safe break point can exceed the target.
Configuration files are planned; the command currently uses the defaults.

Use `--check` or `-c` to report files that would change without writing them:

```sh
mux format --check
mux format -c src
```

The command exits with 0 on success, 1 when a check finds differences, and 2
on an input, syntax, or file-operation error. Formatting only needs valid
syntax. Missing imports and type errors do not prevent formatting.

Directory discovery skips `.git`, `target`, and `node_modules`, and does not
follow symlinks encountered during traversal. Explicit symlink paths resolve
to their targets. Repeated paths are processed once. An empty directory is a
successful no-op; a missing path or an explicit non-Mux file is an error.

The formatter prepares every selected file before writing, so a syntax error
leaves all files untouched. Each changed file is replaced atomically with its
permissions preserved. An I/O failure during replacement can leave earlier
files formatted. Concurrent changes detected during preparation or staging
are reported instead of overwritten; avoid editing files during formatting.
