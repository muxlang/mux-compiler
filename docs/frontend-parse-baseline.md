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
  'syntax_parse/(arithmetic|collections|enums_classes)' \
  --sample-size 50 --warm-up-time 1 --measurement-time 2
./scripts/dev-cargo.sh bench --bench compile -- \
  'syntax_parse_lower/(arithmetic|collections|enums_classes)' \
  --sample-size 50 --warm-up-time 1 --measurement-time 2
```

The measurements below use Criterion's median estimate from the optimized
profile. The corpus contained 171 programs, of which 22 do not compile; each
filtered run measured three fixtures. Syntax-only timing includes lossless
lexing and recording syntax events and facts.

| Fixture | Before moving syntax events | After moving syntax events | Current frontend | Change from prior |
| --- | ---: | ---: | ---: | ---: |
| `arithmetic` | 57.107 µs | 51.904 µs | 44.954 µs | -13.4% |
| `collections` | 981.41 µs | 821.21 µs | 825.71 µs | +0.5% |
| `enums_classes` | 283.43 µs | 211.95 µs | 207.43 µs | -2.1% |

| Fixture | Legacy parser (`60f5254`) | Early syntax path (`60f5254`) | Before move | Current frontend | Current / legacy |
| --- | ---: | ---: | ---: | ---: | ---: |
| `arithmetic` | 25.964 µs | 82.945 µs | 78.352 µs | 58.779 µs | 2.26× |
| `collections` | 409.61 µs | 1.1645 ms | 1.2928 ms | 974.09 µs | 2.38× |
| `enums_classes` | 123.54 µs | 368.04 µs | 377.42 µs | 264.93 µs | 2.14× |

Moving syntax events and typed facts through the tree builder instead of cloning
them improved both syntax-only and parse-plus-lowering times. Removing cached
line and column fields from every span reduced parse-plus-lowering medians by
about 15–17% from the previous measurement. The current path remains about
2.1–2.4 times slower than the former AST parser on these examples. This is a
focused parser comparison, not an end-to-end compile measurement. Peak memory
was not measured.
