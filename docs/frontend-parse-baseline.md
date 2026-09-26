# Frontend parse baseline

This compares the former AST parser with the lossless syntax frontend, including
AST lowering. The syntax frontend retains trivia and typed source facts before
building the compiler AST, so the comparison measures the cost of that added
capability as well as parser throughput.

The historical comparison was measured at compiler commit `60f5254`, before
removing the legacy AST-building path:

```sh
git checkout 60f5254
./scripts/dev-cargo.sh bench --bench compile -- \
  'frontend_parse/(legacy_lex_parse|syntax_parse_lower)/(arithmetic|collections|enums_classes)' \
  --sample-size 30 --warm-up-time 1 --measurement-time 2
```

After the cutover, the legacy parser benchmark was removed. Re-measure the
remaining syntax path with:

```sh
./scripts/dev-cargo.sh bench --bench compile -- \
  'syntax_parse_lower/(arithmetic|collections|enums_classes)' \
  --sample-size 30 --warm-up-time 1 --measurement-time 2
```

The measurements below use Criterion's median estimate from the optimized
profile. The corpus contained 171 compiling programs; each filtered run measured
three fixtures. A parse-only run after cutover separates lossless lexing and
syntax parsing from AST lowering:

| Fixture | Lossless lex + syntax parse | Lossless lex + syntax parse + AST lowering |
| --- | ---: | ---: |
| `arithmetic` | 57.107 µs | 78.352 µs |
| `collections` | 981.41 µs | 1.2928 ms |
| `enums_classes` | 283.43 µs | 377.42 µs |

The parse-only phase includes lossless lexing and recording syntax events and
facts. The remaining time in the second column includes lowering; subtracting
the independently measured medians gives a rough estimate, not a separately
measured lowering benchmark.

| Fixture | Legacy parser (`60f5254`) | Early syntax path (`60f5254`) | Syntax path after AST cutover | Current / legacy |
| --- | ---: | ---: | ---: | ---: |
| `arithmetic` | 25.964 µs | 82.945 µs | 78.352 µs | 3.02× |
| `collections` | 409.61 µs | 1.1645 ms | 1.2928 ms | 3.16× |
| `enums_classes` | 123.54 µs | 368.04 µs | 377.42 µs | 3.06× |

The syntax frontend remains about three times as slow on these examples as the
former AST parser. Removing parser-side AST construction improved the arithmetic
fixture by about 6%; collections regressed by about 11%, and enums/classes by
about 3% against the early syntax path. This is a focused parser comparison,
not an end-to-end compile measurement. Peak memory was not measured, and the
collections regression still needs investigation.
