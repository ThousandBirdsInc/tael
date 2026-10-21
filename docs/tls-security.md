# October 2026 TLS security update

The lockfile includes `rustls-webpki` 0.103.16. Its verifier rejects paths
containing the distrusted intermediate public keys, including alternate
cross-signs. Updating a root bundle alone is not equivalent to this verifier fix.

- [Upstream verifier release](https://github.com/rustls/webpki/releases/tag/v/0.103.16)
- [Mozilla incident report](https://bugzilla.mozilla.org/show_bug.cgi?id=2083449)
- [Trust-store operator guidance](https://groups.google.com/a/mozilla.org/g/dev-security-policy/c/0R3XLJL6FuE)

## Trust and compatibility

This update intentionally selects Rustls for repository-owned HTTP clients.
Merely enabling a Rustls feature is insufficient: reqwest's default TLS feature
or dependency feature unification can otherwise select native-tls. The manifest
therefore disables reqwest defaults and explicitly retains its non-TLS default
features (`charset`, `http2`, `system-proxy`). This also prevents re-enabling
native TLS in dependencies that use `Client::new()` themselves.

The clients continue to load native/system roots, including installed private
CAs. Bundled public roots are explicitly disabled before native roots are
enabled, so another dependency enabling webpki roots does not silently expand
these clients' trust anchors. This does not edit the machine's trust store,
disable certificate/hostname validation, or remove unrelated roots. Existing
proxy discovery, authorization headers, request timeouts and HTTP behavior are
preserved.

Native root loading is not identical to the platform verifier: platform-specific
revocation rules, enterprise trust constraints and automatic intermediate
retrieval can differ. Validate private-CA endpoints and any TLS-intercepting
proxy on each deployed OS before rollout. These code paths did not configure
client-certificate identities, and this change does not introduce or migrate
identity handling.

## Deployment checklist

1. Rebuild and reinstall binaries/images using the updated lockfile and restart
   existing processes. A source PR does not patch a running or previously
   distributed binary.
2. Confirm `cargo tree --locked -i rustls-webpki` selects 0.103.16 or a later
   patched compatible release. Libraries embedded elsewhere use the
   application's own lockfile; update and rebuild that application too.
3. Keep operating systems, CA packages, browsers and external command-line
   clients updated independently. Rustls's SPKI distrust does not patch an
   external native-TLS client or an unrelated runtime.
4. Exercise normal valid HTTPS and intended private-CA/proxy endpoints, then
   reject both direct eMudhra and AAA cross-signed chains from the
   [incident test hosts](https://emudhra.we-have-a-problem.com/). A timeout, DNS
   failure or proxy-generated certificate is not proof of the expected
   certificate rejection.

The opt-in `rejects_compromised_emudhra_chains` test requires both a valid HTTPS
control and the specific `CertDistrusted` validation error. It is ignored in the
ordinary suite because it depends on third-party public hosts remaining
available and on an unintercepted TLS connection. It does not disable normal
certificate checks or transmit application credentials.

## Bundled roots and RUSTSEC-2026-0381

The optional cloud dependency graph also locks `webpki-roots` 1.0.10, which
removes the affected emSign ECC Root CA G3 per
[RUSTSEC-2026-0381](https://rustsec.org/advisories/RUSTSEC-2026-0381.html).
The transitive 0.26.11 compatibility crate only re-exports `TLS_SERVER_ROOTS`
from the same patched 1.0.10 crate; it has no independent root list. An advisory
scanner that only compares the shim's version to `>= 1.0.10` may still flag it.
The actual root contents and the SPKI verifier are both updated; this change
does not suppress advisories or replace the private/native roots used by owned
HTTP clients. Keep the verifier fix as well, because alternate cross-signs are
not solved by removing only one root.

## Tael-specific coverage

The explicit backend applies to the CLI (including SSE), GUI, alert webhooks,
remote-store reads and remote WAL writes. Unix-socket clients retain their
socket routing. Optional cloud object storage and Postgres Rustls paths also
use the patched lockfile; their configured trust settings are unchanged.

Run the live regression with:

```sh
cargo test --locked -p tael-cli rejects_compromised_emudhra_chains -- --ignored
```

Validate default, cloud and GUI builds separately. Updating this verifier does
not demonstrate that any particular deployment contacted an affected chain,
and no credential or private-CA rotation is implied by the dependency alone.
