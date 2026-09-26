# Frontend parse baseline

This records the compatibility parser cost before removing its AST-building
path. Both measurements include lexing and produce a compiler AST: the legacy
path lexes and parses directly, while the syntax path parses losslessly and
lowers the result.

Reproduce with:

```sh
./scripts/dev-cargo.sh bench --bench compile -- \
  'frontend_parse/(legacy_lex_parse|syntax_parse_lower)/(arithmetic|collections|enums_classes)' \
  --sample-size 30 --warm-up-time 1 --measurement-time 2
```

Measurements below use Criterion's median estimate from the optimized profile.
The corpus contained 171 compiling programs; this filtered run measured three
fixtures.

| Fixture | Legacy lex + parse | Syntax parse + lower | Ratio |
| --- | ---: | ---: | ---: |
| `arithmetic` | 25.964 µs | 82.945 µs | 3.19× |
| `collections` | 409.61 µs | 1.1645 ms | 2.84× |
| `enums_classes` | 123.54 µs | 368.04 µs | 2.98× |

The syntax frontend takes about three times as long on these examples. This is
a focused baseline, not an end-to-end compile comparison. Peak memory was not
measured; investigate the cost before treating this path as complete.
