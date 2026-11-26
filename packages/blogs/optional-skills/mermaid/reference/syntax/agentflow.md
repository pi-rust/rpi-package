# Agentflow (Mermaid 12)

Condensed from the open-source syntax page. Keyword: `agentflow-beta`. Beta: the keyword and
details can change. Full page: https://mermaid.ai/open-source/syntax/agentflow.html

Prefer IR `graph` / `agentflow-beta` ([ir-catalog.md](../ir-catalog.md)) for nodes, flows, and
the three edge kinds. Hand-author the constructs in the last section.

```
agentflow-beta LR
    flow reviewer["Review Agent"]
        changes["Gather changes"]@{ shape: input }
        analyse["Analyse diff"]@{ shape: task }
        lint["run_linter"]@{ shape: tool }
        ok["Clean?"]@{ shape: decision }
        changes --> analyse --> lint --> ok
    end
```

Direction after the keyword: `TB`, `TD`, `BT`, `LR`, `RL`.

Shapes (aliases; a normal flowchart shape name also works):

| `shape` | Meaning |
|---|---|
| `task` | Work an agent performs |
| `tool` | Callable capability |
| `input` | Data entering the flow |
| `decision` | Branch |
| `refdoc` | Reference material |
| `action` | Side effect (send, write, publish) |

Edges:

| Operator | Meaning |
|---|---|
| `-->` | Sequence (control or data) |
| `-.-` | Reference (no control passes) |
| `--x` | Failure path |

Labels sit in the arrow: `check -- yes --> ship`. A `flow id["label"] … end` container nests.
Flow ids are edge endpoints (`researcher --> writer`).

Hand-author, do not expect the IR to emit:

- Metadata: `@{ model, instruction, params, returns, connectorRef, description }`.
- `connector id["label"]` plus `connectorRef`.
- `global … end` for a node shared by several flows.
- `flowId@{ view: "collapsed" }`.
