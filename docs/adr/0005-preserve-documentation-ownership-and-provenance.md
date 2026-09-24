---
status: proposed
---

# Preserve documentation ownership and provenance across a future re-fork

Via inherits a zkSync documentation book while maintaining its own guides, research, and decisions.
Rewriting inherited Ethereum contracts as Bitcoin behavior hides their origin and makes future
upstream comparisons harder. Freezing every inherited page can also mislead readers about Via.

## Proposed decision

Keep inherited explanations under `docs/src/` identifiable as zkSync material. Put divergent Via
behavior in the existing Via-owned documents rather than rewriting the upstream contract in place.
Use the [existing documentation taxonomy](../research/README.md#where-information-belongs) to choose
the owner. Do not create another documentation tree or copy the research into an implemented guide.

Allow small applicability notices and corrections to genuinely shared behavior in inherited pages.
Link a notice to the Via document that owns the difference. Such edits need a recorded purpose and
source revision so a later upstream sync can reassess them rather than silently drop or copy them.
The mechanism for tracking those edits remains open.

A future re-fork must review both sets of documents. Preserve accepted Via contracts and their
evidence, then check their implementation references against the selected upstream revision.
A path outside `docs/src/` does not make a document survive automatically, and preserving a file
does not prove that its behavior still holds. The [re-fork research](../research/via-contracts-and-a-future-refork.md)
explains that distinction.

## Alternatives and tradeoffs

Editing all inherited prose in place gives readers one narrative but mixes ownership and enlarges
the upstream diff. A full parallel book avoids those edits but duplicates shared material and adds
another publication system. A blanket freeze preserves the upstream text at the cost of misleading
applicability. Small notices with Via-owned explanations limit duplication, but maintainers must keep
links and applicability claims accurate through each sync.

## Scope still open

This is a draft, not an accepted documentation policy or an approved re-fork.
[The current book configuration](../book.toml) uses `docs/src/`, the zkSync title, and upstream edit
links. Choosing a Via publication target, book title, edit URLs, navigation changes, and a process for
tracking inherited-page exceptions requires a separate decision. This ADR changes none of them.
It does not authorize publication of private evidence or changes to deployment workflows.
