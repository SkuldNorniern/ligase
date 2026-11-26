# third party

what ships inside ligase besides its own code, and what each license asks.

## native code

- **ring** (`Apache-2.0 AND ISC`): crypto for tls on linux only, partly from BoringSSL. ships compiled in there: keep its license and notices with the linux binaries. windows and macos use the system's tls instead.

## rust crates with duties past MIT/Apache

- **webpki-roots** (`CDLA-Permissive-2.0`): the Mozilla root certificates the built-in agent's https checks against on linux. keep the license text with the linux binaries.
- **rustls** (`Apache-2.0 OR ISC OR MIT`), **ureq** (`MIT OR Apache-2.0`): the built-in agent's https; rustls on linux only. MIT or Apache: keep the copyright notice.
- **native-tls** (`MIT OR Apache-2.0`), **schannel** (`MIT`) on windows, **security-framework** (`MIT OR Apache-2.0`) on macos: https through the system's own tls and root store. keep the copyright notice.

- **hayro** and its parts (`hayro-syntax`, `-interpret`, `-font`, `-cmap`, `-jpeg2000`, `-jbig2`, `-ccitt`; `Apache-2.0 OR MIT`): draws pdf pages, pure rust. `hayro-font` carries the Foxit base 14 fonts taken from pdfium (`BSD-3-Clause`): keep their notice. `hayro-interpret` carries a CGATS icc profile under its own permissive terms (`assets/CGATS_LICENSE.txt` in the crate). **brotli** (`BSD-3-Clause`) unpacks its cmaps.

## build only, not shipped

- **winres** (`MIT`): puts the icon in the windows exe at build time.

the rest of the dependency tree is MIT or Apache-2.0: `cargo tree` lists it, and the copyright notices go with the binaries.
