# Formatting Mux source

Run `mux format` from a project directory to format its `.mux` files
recursively. You can also pass files or directories:

```sh
mux format
mux format src tests
mux format ~/test.mux
```

The formatter reads `mux-project.json` from the working directory or the
nearest parent directory. It stops after checking the nearest Git worktree
root, and never checks `/`. A command uses one config for all its paths. If no
config is found, built-in defaults apply. Formatting through the Rust library
does not read project config; callers can pass `FormatOptions` directly.

Put formatter settings under `format`:

```json
{
  "format": {
    "indent_type": "space",
    "indent_count": 4,
    "line_width": 80,
    "brace_style": "same_line",
    "where_position": "own_line",
    "blank_lines_between_declarations": 1,
    "blank_lines_between_members": 0,
    "blank_lines_before_functions": 1,
    "trailing_comma": "multiline"
  }
}
```

The defaults are four spaces per indentation level, an 80-column target,
opening block braces on the declaration line, and `where` clauses on their own
line. There is one blank line between top-level declarations, no blank line
between ordinary class/type members, and one blank line before each function
member. Blank-line settings accept any nonnegative count. Set `indent_type` to
`"tab"` for tabs; its default `indent_count` is one tab per level.

`brace_style` accepts `"same_line"` or `"next_line"`. `where_position` accepts
`"own_line"` or `"same_line"`. `trailing_comma` accepts `"multiline"`,
`"never"`, or `"always"`; it applies where the Mux grammar permits a trailing
comma in lists, maps, and match arms. Multiline wrapping is automatic and uses
the configured width. Long expressions may wrap after binary operators. The
operator stays at the end of its line; a line may not start with an operator,
and a line break after declaration `=` is not allowed.

Invalid JSON falls back to all defaults. Invalid settings use their individual
defaults, and unknown fields are ignored. Each issue is reported as a warning
on stderr; formatting continues. The formatter always supplies a policy for
layout. It keeps literal spellings, comment contents, and declaration order.
Generated structural line breaks use LF; line endings inside literals and
comments are preserved. Long literals, comments, and expressions without a
safe break point can exceed the width target.

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
