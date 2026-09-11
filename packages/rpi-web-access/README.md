# rpi-web-access

A bounded HTTP(S) fetch extension inspired by `pi-web-access`. It registers
`web_fetch`, follows at most five redirects, rejects credential-bearing and
private-IP URLs, enforces a timeout, and truncates large responses.
