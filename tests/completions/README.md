# Completions HTTP round-trip scenario

This scenario proves that the [Completions](https://github.com/atgreen/cl-completions)
library performs a real HTTP request/response cycle on TorCL — JSON encode,
socket write, HTTP parse, JSON decode, and the assistant text pulled back out of
the decoded alist. It imports every TorCL compatibility fork by immutable Git
commit and pins the rest of the graph by digest; it never patches a downloaded
registry copy.

From the TorCL repository:

```sh
OCICL_BIN="$HOME/git/ocicl/ocicl" \
OCICL_RUNTIME="$HOME/git/ocicl/runtime/ocicl-runtime.lisp" \
TORCL_BIN="$PWD/target/torcl" \
SBCL_BIN="$(command -v sbcl)" \
  bash scripts/test-completions.sh
```

The defaults are `ocicl` on PATH, its installed runtime under
`${XDG_DATA_HOME:-$HOME/.local/share}/ocicl/`, and `target/torcl`. The ocicl
binary must support `git+` sources.

Each run creates and retains a private temporary project with the committed
`ocicl.csv`. `ocicl install` fetches its pins. `fake-ollama.py` then binds an
Ollama-shaped `/api/chat` server on an ephemeral loopback port, exports the port
as `TORCL_OLLAMA_PORT`, and runs the Lisp under it:

- Cold (gated): compile and load the graph (455 files), then `dex:post` the
  endpoint and assert a 200 with the echoed prompt in the body, then load
  `:completions` and assert `GET-COMPLETION` returns the assistant text and a
  two-turn history.
- Cached (`TORCL_COMPLETIONS_CACHED=1`, currently expected to FAIL — see below):
  repeat the assertions and reject source recompilation.
- Optional `SBCL_BIN`: run the same checks on SBCL with a separate cache.

The server **echoes the prompt back** as the assistant's reply, so a malformed
request, a dropped byte, or a mis-keyed decode fails an assertion rather than
passing silently. Three real defects were found exactly this way (all now fixed):

- `TYPEP` ignored an array specifier's element type, so Dexador wrote its
  User-Agent *string* into the binary request buffer (`bliss-ubb2`).
- The bytecode lowerer hoisted a LOOP `until` above an earlier `do`, so
  `READ-UNTIL-CRLF*2` dropped every CR of the response (`bliss-4tu2`).
- `INTERN` minted a second KEYWORD symbol, so cl-json's decoded keys were not
  `EQ` to the keyword literals Completions writes (`bliss-r8kt`).

## Known cached-phase failure (bliss-6d8f)

The cached phase currently fails, on a defect unrelated to the fixes above, so it
is opt-in rather than gated. Once pure-tls has been loaded, the upfront
COMPILE-FILE read of ironclad's `src/digests/whirlpool.lisp` is corrupted — the
form handed to the `#.` on line 149 arrives as `(0 . 0)`, i.e. zeroed nursery
memory, and the compile reports `undefined function: Fixnum(0)`. COMPILE-FILE
then downgrades that one file to a source-only artifact, which loads correctly in
the compiling process but not in a fresh one: replaying the text skips the
`(eval-when (:compile-toplevel) …)` block that defines its S-box helpers, so the
cached load dies with `undefined function: S`.

To see it, run the check twice in a retained artifact directory:

```sh
cd <artifact dir>
export OCICL_LOCAL_ONLY=1 TORCL_PORT_RUNTIME=... TORCL_PORT_CACHE="$PWD/cache/"
/path/to/torcl/scripts/torcl-limited.sh python3 fake-ollama.py \
    /path/to/torcl --no-init --load check.lisp
```

Compiling ironclad on its own produces a correct fasl every time, so the trigger
is the extra heap pressure from the pure-tls load, not the file. The failure is
silent without `TORCL_BFASL_TRACE=1` (bliss-wk2q).

A read timeout on the socket also surfaces as a raw
`Resource temporarily unavailable` stream error rather than a timeout condition
(bliss-xm6i); it showed up once in the cold phase while the machine was loaded.

`pure-tls/cl+ssl-compat` is loaded and `cl+ssl` registered immutable so nothing
pulls the CFFI-based original; the endpoint is plain HTTP either way, so this is
**not** a claim of HTTPS support. Nor is it a claim that Completions' whole
upstream suite passes — that suite currently reports 66 of 67 checks passing on
TorCL, the exception being the Gemini tool-name baseline (`bliss-0xlb`), which
SBCL fails identically.

Commands are memory/time capped by `scripts/torcl-limited.sh`; the cold phase
raises the defaults to `TORCL_MEM_MAX=8G` and `TORCL_TIMEOUT=2400` because
compiling the graph is slow.

Refresh pins deliberately with `ocicl install git+URL@SHA` in an isolated
project, rerun this scenario, and update the committed CSV and
[ports/README.md](../../ports/README.md). Do not submit upstream patches or PRs
without new authorization.
