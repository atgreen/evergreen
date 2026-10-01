# Ironclad regression

The pinned dependency graph exercises compilation and loading through ASDF,
Whirlpool's nested local macros, and all six upstream EAX, ETM and GCM vector
groups (whole-message and incremental operations).
The runner installs dependencies into a temporary directory and uses a fresh
FASL cache. It also checks both CL-PPCRE/Ironclad load orders, first with
fresh compilation and then cached FASLs in a new process. Both the parser
function and Ironclad's key-group reader must remain callable. It preserves
that directory and logs for diagnosis.

Run against an installed or saved-image executable:

```sh
EGCL_BIN=/path/to/egcl scripts/test-ironclad.sh
```

The runner requires `ocicl` and its installed runtime. `OCICL_BIN`,
`OCICL_RUNTIME`, `EGCL_MEM_MAX`, and `EGCL_TIMEOUT` override their defaults.
A successful run prints `IRONCLAD-PASS`. An error or timeout is a failure;
the runner does not suppress test failures. Set `EGCL_IRONCLAD_FULL_TESTS=1`
to run all 461 upstream tests instead of the six targeted vector groups; this
can take substantially longer.
