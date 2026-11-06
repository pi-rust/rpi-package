These DER files are a public, self-signed localhost TLS test identity. The key
is test data, never used for a deployed service. Only test-specific HTTP clients
trust this certificate; production native package downloads use normal TLS roots.
