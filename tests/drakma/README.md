# Drakma over EGCL's native TCP backend

Run `bash scripts/test-drakma.sh` with `EGCL_BIN` pointing to a freshly built
EGCL executable. The script installs the immutable sources in `ocicl.csv` into
a temporary project, disables runtime downloads, and checks cold and cached
loads against a local HTTP server. Both GET and POST must preserve the request
path, request body, response status, and response body. A cached load must not
compile source again.

The scenario uses Drakma's `:drakma-no-ssl` feature and tests plain HTTP only.
It does not validate TLS.

If Drakma reports that `USOCKET::SOCKET-CONNECT-INTERNAL` is undefined, check
which USOCKET ASDF found:

```lisp
(asdf:system-source-directory :usocket)
(fboundp 'usocket::socket-connect-internal)
```

The fork's default `master` branch has no EGCL backend. Install its EGCL branch
from the application's project directory, then restart EGCL:

```sh
ocicl install git+https://github.com/atgreen/usocket@egcl-support
```

`install-egcl-forks` selects this branch too. Commit the resulting `ocicl.csv`
to retain the resolved revision. Use a current EGCL containing the Gray-stream
predicate, EOF, binary sequence, and type-alias fixes: Drakma wraps native
sockets in Chunga and Flexi Streams.
