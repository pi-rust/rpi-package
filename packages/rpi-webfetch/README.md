# rpi-webfetch

Fetch public HTTP(S) pages and return bounded readable text. Private hosts,
embedded credentials, and oversized responses are rejected. Hostname DNS
resolutions that point at private/local address ranges are blocked, and fetch
failures are returned as model-readable tool results so one bad URL does not
abort the whole agent turn.
