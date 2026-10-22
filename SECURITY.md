# Security Policy

## Supported versions

| Version | Supported |
|---|---|
| 3.x (latest release) | ✅ |
| < 3.0 | ❌ (upgrade to the latest release) |

## Reporting a vulnerability

Please **do not** open a public issue. Use GitHub's
[private vulnerability reporting](https://github.com/grovedock/gd/security/advisories/new).
You will receive an acknowledgement within 72 hours (and in any case within 14 days) and a
fix timeline after triage.

Please include the affected version (`gd --version`), the component (client, server,
SDK, console), steps to reproduce and the impact you expect. We fix confirmed medium or
higher severity issues within 60 days, coordinate the disclosure date with the reporter,
publish a GitHub security advisory, and list the fix, with its CVE or advisory id, under
**Security** in [CHANGELOG.md](https://github.com/grovedock/gd/blob/main/CHANGELOG.md) and the release notes.

## Security model (v1)

* **Authentication**: bearer tokens (`GD_TOKENS`, `principal:token` pairs), compared in
  constant time. Run with TLS (`--tls-cert/--tls-key`) whenever traffic leaves the host.
  Without `GD_TOKENS`, OIDC or platform tokens the server allows anonymous access, logs a
  warning, and refuses to listen beyond loopback unless `--allow-anonymous` is set.
* **SSO**: OpenID Connect bearer tokens (`--oidc-issuer/--oidc-audience`). Keys come from the
  issuer's JWKS and rotate automatically; only asymmetric algorithms matching the key are
  accepted (no `alg: none`, no HMAC key confusion); `iss`, `aud` and `exp` are required. The
  principal is a configurable claim and works with the access rules below. Static tokens and
  OIDC can be enabled together.
* **Authorization**: per-directory **read** and write permissions and per-ref permissions
  (`--acl rules.json`). Principals with path-scoped read access never read objects by id:
  the server denies `Objects.Get/Missing` to them and serves filtered views instead (path-
  scoped checkout, browse, find, diff, and redacted history). Regional caches and node daemons
  serve objects by id without a policy, so expose them only to full-read principals.
* **Integrity**: every object is hash-verified when accepted by a server, when received
  by a tier from upstream, and when materialised by a client.
* **Client filesystem safety**: paths are validated (`..`, `.gd`, separators, NUL) and no
  write follows a symlink, so a malicious tree cannot write outside the working copy.
* **Resource limits**: per-object size cap (256 MiB; larger files are chunked) with bounded decompression; bounded
  ids per request, history scan, find and blame work per request.

## Audit trail

Run the server with `--audit-log FILE` to record who read or changed what, from where,
with what result, including rejected requests. The format is described in the
[security guide](https://grovedock.com/docs/security.html#audit-log).

## Verifying a release

From 2.1.1 on, every release archive carries a signed SLSA build provenance attestation
and an SPDX software bill of materials, both signed through Sigstore by the release
workflow. Check an archive before installing it:

```sh
gh attestation verify gd-v2.1.1-x86_64-unknown-linux-gnu.tar.gz --repo grovedock/gd
```

Container images on `ghcr.io/grovedock/grovedock` carry build provenance and an SBOM as
OCI attestations (`docker buildx imagetools inspect --format '{{ json .Provenance }}' IMAGE`).
Each archive also ships a `.sha256` checksum, which `install.sh` verifies.
