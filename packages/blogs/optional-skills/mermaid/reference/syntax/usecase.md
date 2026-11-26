# Use case diagrams (Mermaid 12)

Condensed from the open-source syntax page. Keyword: `usecase-beta`. Put each statement on
its own line. Full page: https://mermaid.ai/open-source/syntax/usecase.html

Prefer IR `usecase` / `usecase-beta` ([ir-catalog.md](../ir-catalog.md)) for actors, use cases,
boundaries, associations, include, extend, and generalization.

```
usecase-beta
    direction LR
    actor Customer("Customer")
    systemBoundary orders("Order system")
        Checkout("Place order")
        Payment("Pay")
    end
    Customer --> Checkout
    Checkout ..> : include Payment
```

- Direction: `TD`, `TB`, `BT`, `LR`, `RL`, on its own line.
- Ids match `[A-Za-z0-9_]+` and are shared by actors, use cases, and boundaries.
- `actor Id` or `actor Id("Label")`. A use case is `Id("Label")` (ellipse) or `Id[Label]` (rectangle).
- An undeclared relationship endpoint becomes an ellipse use case. Actors are never inferred.
- `systemBoundary id("Title")` … `end`, or `systemBoundary id` / `systemBoundary "Title"`.
- Association: `A --> B` or `A -- "label" --> B`. A label that happens to say "include" is still an association.
- Include and extend, both ends use cases: `Checkout ..> : include Payment` and `ApplyCoupon ..> : extend Checkout`. The colon has a space before it (`..> :`).
- Generalization, two actors or two use cases: `Admin --|> Person`.

Hand-author, do not expect the IR to emit: notes, JSON tables, stereotypes (`<<…>>`), actor
`@{ type: hollow }`, icons, or `systemBoundary` package metadata.
