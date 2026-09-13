# rpi-firecrawl

Firecrawl-backed web tools for the `rpi` agent: `firecrawl_search` and
`firecrawl_scrape`.

Firecrawl converts web pages into clean, LLM-ready Markdown and handles
JS-heavy sites, proxies, and rate limits automatically.

## Keyless by default

Firecrawl v2 works without an API key (lower rate limits). For higher rate
limits set the environment variable:

```bash
export FIRECRAWL_API_KEY=fc-your-key
```

To point at a self-hosted Firecrawl instance instead of the hosted API:

```bash
export FIRECRAWL_BASE_URL=http://10.100.100.2:3002
```

## Tools

- `firecrawl_search(query, limit, content)` — web search returning titles,
  URLs, descriptions, and (optionally) full page Markdown.
- `firecrawl_scrape(url, maxChars)` — convert a public URL to clean Markdown.
