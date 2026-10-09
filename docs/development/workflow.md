# Development workflow

How work reaches main. Work happens on local branches; main moves only by
merging a branch that passed every check below, or that changes only
documentation.

## 1. Before merging to main

On the branch's tip, rebased on main, so that what is checked is what main
becomes:

```sh
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo nextest run --workspace
cargo nextest run --workspace --profile fuzzy
```

Then, once all four pass:

```sh
git checkout main
git merge --ff-only <branch>
```

A branch that changes only Markdown files (the documents under `docs/`,
`AGENTS.md`) skips the four checks, since nothing it touches is built or
tested, and is merged the same way. Doc comments in `.rs` files are not
documentation in this sense: they are code, and clippy checks them.

## 2. The two test suites

The suites are those of skein's `docs/foundation/testing-strategy.md`
(section 8); `docs/design/domain/testing.md` (section 9) says what they
hold in jig.

- **The default suite** is every crate's unit tests and each world's
  focused tests (`tests/<name>/tests/*.rs`): focused tests of
  expected behaviour (scenarios, referee tests, replay, facts changing
  nothing, memory against the worst case), and a cheap random world as a
  smoke test where one helps. It is what `cargo nextest run --workspace`
  runs, and it takes at most **15 seconds**.
- **The fuzzy suite** is the worlds' `tests/fuzzy_*.rs`: randomized tests,
  such as sweeps of random worlds and domains driven at random, over many
  seeds. It is not run by default, only with `--profile fuzzy`, as the
  gate before merging to main, and it takes at most **1 minute**.

The budgets are enforced, not advisory: `.config/nextest.toml` gives each
profile a `global-timeout`, so a suite that runs past its budget fails,
naming the tests it stopped, and a slow period past which a test is
flagged SLOW as it runs. A change that breaks a budget is fixed by making
tests cheaper, or by moving randomized ones to a world's fuzzy tests.
Raising a budget is a decision to take explicitly, not a fix. To see what
each test costs on its own, the `measure` profile has no budget:

```sh
cargo nextest run --workspace --profile measure -j 1
```

The budgets are wall time on the development machine (4 cores, 8 threads,
which nextest uses all of), with nothing else building. Run the checks on
an idle machine.

## 3. When a fuzzy seed fails

A failure names its seed, and a seed replays to the same run. Fix the bug,
then keep its seed: as a scenario among its world's focused tests if it
shows behaviour worth naming, or among the sweep's pinned seeds. A finding
that cannot be fixed yet joins its world's `FINDINGS`, which an ignored
test replays until it is fixed.
