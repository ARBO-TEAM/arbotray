# MIT License

Copyright (c) 2026 ARBO-TEAM

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.

---

## What this covers

The ArboTray source and the binaries built from it. There is no separate
commercial edition to license — every feature in this repository is under the
terms above, and nothing in the app is gated, metered or held back.

## Third parties

ArboTray links the `windows` crate, which
is MIT licensed, along with `serde` and `serde_json` (MIT OR Apache-2.0). The
release binary statically links them, so their notices are reproduced here
rather than only in `Cargo.lock`.

Everything else it talks to is the operating system: Win32 through the
`windows` crate, and — for the speed test and the update check, both of which
are only ever started on request — `speed.cloudflare.com` and `api.github.com`.
