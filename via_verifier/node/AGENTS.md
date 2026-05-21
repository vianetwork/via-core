# AGENTS.md — via_verifier/node/

STOP before adding or changing any detector, poller, watcher, client,
DAL method, parser, conversion, or fetch/compare logic in this tree.

Required actions before writing implementation code:

1. Run a search across the sibling tree (core/node/). Example:
   rg -n 'fn |struct ' core/node/ | rg -i '<your_keyword>'

2. Read at least one matching file in core/node/ before proceeding.
   For reorg-related work, this MUST include core/node/via_main_node_reorg_detector/.

3. If shared logic exists or could exist, extract it to core/lib/ (or the most appropriate shared crate) rather than duplicating here. Shared logic does **not** belong in via_verifier/node/.

4. In your PR's "Reuse & Duplication" section, record:
   - Sibling paths inspected
   - Closest existing owner considered
   - Search command run
   - If not shared: the exact invariant or execution-context difference that prevents extraction

If you cannot complete steps 1–3, do not write the implementation. Search first.

Canonical anti-pattern this prevents: logic duplicated between main-node and verifier instead of being extracted to a shared location.
