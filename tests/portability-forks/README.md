# Library portability adapters

From the repository root:

```sh
OCICL_BIN=/usr/bin/ocicl EGCL_BIN=/path/to/egcl scripts/test-portability-forks.sh
```

Use an ocicl build with working Git-source installation. The scenario installs
immutable revisions from `ocicl.csv` into a fresh directory and disables runtime
downloads and inherited ASDF registries. Logs and generated archives remain in
the printed artifact directory. Every process runs through `egcl-limited.sh`.

The checks cover:

- Cold and cached ASDF loads of Archive, ZIP, unix-opts, with-user-abort,
  local-time and Parachute.
- All seven Archive stat accessors, a missing-file error, TAR entry creation
  and reading, and ZIP pathname entry writing and reading. Python independently
  verifies both archive payloads and permission bits.
- Raw argv and real process exit statuses (unix-opts 23, Parachute 24).
- Translation of an EGCL interrupt condition into `user-abort`. This signals
  the runtime condition directly; it does not inject an OS signal.
- Local-time's Unix epoch conversion and subsecond precision, bounded between
  two native clock samples.

These adapters require `EGCL-POSIX`, `EGCL-EXT:GET-PRECISE-TIME`,
`EGCL-EXT:RAW-COMMAND-LINE-ARGUMENTS`, and `EGCL-EXT:INTERRUPT-CONDITION`.
They do not add Archive descriptor-based stat or Parachute timeout enforcement.
Full upstream test suites and moving-GC stress are outside this scenario.
The conditional-dispatch regressions also live in the forks; each was observed
failing before its corresponding adapter was added.
