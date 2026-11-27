---
default: minor
---

# Relationship diagrams draw foreign keys as curved lines with crow's-foot ends (optional, mandatory, one-to-one). Lines are curved by default; the header View menu switches between curved and smooth (rounded orthogonal) lines per diagram, next to the card detail; lines entering the same table get their own vertical. Hovering a table highlights its relations and neighbours and dims the rest; hovering a line highlights its key columns and shows `orders.customer_id → customers.id` with the constraint name. A minimap (bottom right, can be turned off) pans the view, Ctrl+F finds a table and flies to it, cards can show all columns, keys only or headers only (also per card with the chevron), and Auto arrange produces fewer crossings.
