# rpi-memory

Local persistent memory inspired by Pi memory extensions. The `memory` tool
supports `remember`, `recall`, `list`, `forget`, and `clear`; entries are stored
in `<cwd>/.rpi/memory.json` and recall uses deterministic token-overlap scoring
so it works without a hosted vector database.
