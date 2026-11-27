---
default: patch
---

# Relationship diagram: long column names no longer overlap their type

When a column's name and type do not both fit the card, the name is cut with an ellipsis (never below half the row) and then the type, instead of painting over each other. The SVG and PNG export does the same.
