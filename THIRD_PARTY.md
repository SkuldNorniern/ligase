# third party

what ships inside ligase besides its own code, and what each license asks.

## native code

- **ring** (`Apache-2.0 AND ISC`): crypto for tls, partly from BoringSSL. ships compiled in: keep its license and notices with the binaries.

## rust crates with duties past MIT/Apache

- **webpki-roots** (`CDLA-Permissive-2.0`): the Mozilla root certificates the built-in agent's https checks against. keep the license text with the binaries.
- **rustls** (`Apache-2.0 OR ISC OR MIT`), **ureq** (`MIT OR Apache-2.0`): the built-in agent's https. MIT or Apache: keep the copyright notice.

## build only, not shipped

- **winres** (`MIT`): puts the icon in the windows exe at build time.

the rest of the dependency tree is MIT or Apache-2.0: `cargo tree` lists it, and the copyright notices go with the binaries.
