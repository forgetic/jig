# The recorder

Reference for `jig record` and the `jig_record::Recorder` API. For *why* the
fixtures come from recorded traffic, read
[record-and-conform](../explanation/record-and-conform.md).

`jig record` is a passthrough recorder: it proxies a real client ↔ real backend
exchange to redacted, client/role-tagged fixtures so the scripted replies
can be derived from ground truth. Recording is manual (it needs a live API key
and network); the captured taxonomy and workflow live in
[`crates/jig-record/README.md`](../../crates/jig-record/README.md).

From Rust, the recorder is `jig_record::Recorder`, built like `FakeLlm`: one
loop thread over a loopback listener, serving client connections concurrently.
Paths that are not a dialect route (connectivity preflights such as `HEAD /`)
get `204`. Dialect routes are forwarded over HTTPS, and the response is relayed
back unbuffered while it is captured. `RecorderConfig::mode` sets how many
exchanges it captures:

- `Mode::Once` (the default) stops after the first routable exchange ends, and
  captures it if it completed. `next_capture(timeout)` returns that capture. If
  the exchange failed, `next_capture` fails as soon as the recorder stops, and
  stderr says why. `jig_record::record_once` is this mode plus writing the
  fixture; `jig record` calls it.
- `Mode::Pump` captures every routable exchange until `stop()`, which returns
  the captures not yet taken. Relays still in flight get a 1 s grace and are
  captured if they finish. The capture examples use this mode for clients that
  make several requests.

```rust
use jig_record::{Mode, Recorder, RecorderConfig};

let recorder = Recorder::start(RecorderConfig { mode: Mode::Pump, ..RecorderConfig::default() })?;
let url = recorder.base_url(); // point the client here
// ... drive the client ...
let exchanges = recorder.stop(); // Vec<(ClientRequest, UpstreamResponse, Route)>
```

`RecorderConfig::upstream_host` forwards to another host than the dialect's
own (DeepSeek or a gateway for the OpenAI dialect). `upstream` (connect to this
address, with or without TLS) and `roots` (trust these CA roots instead of
webpki's) are test hooks.

To refresh fixtures in one command, `xtask record` orchestrates the recorder
across the scenario matrix, and `xtask staleness` reports how old the committed
captures are:

```sh
cargo run -p xtask -- record --dialect openai   # one dialect (--all for everything)
cargo run -p xtask -- staleness                 # offline: flag captures past N days
```

See [the refresh how-to](../how-to/refresh-fixtures.md) for the full procedure
and [the record-and-conform explanation](../explanation/record-and-conform.md)
for the design.

