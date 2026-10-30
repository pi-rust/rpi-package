# TEST ONLY TLS material

Generated locally with the already-installed OpenSSL; no dependencies installed.
`ca.pem` is a test-only self-signed CA. Its private signing key was deleted.
`server-key.pem` is a deliberately public, non-secret TEST ONLY private key.
Never trust this CA or deploy this key outside isolated test fixtures.

`server.pem` is signed by this CA with serverAuth EKU, DNS SAN
`oauth.example.invalid` and IP SAN `127.0.0.1`. Validity:
2026-10-30 03:38:19 UTC through 2036-10-27 03:38:19 UTC.

Tests use an explicit ring crypto provider with TLS certificate chain and
hostname verification enabled. The caller explicitly adds the CA through
`with_root_certificates`; no invalid-certificate or invalid-hostname bypass exists.
