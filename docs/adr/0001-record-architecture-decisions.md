# ADR-0001: Record architecture decisions

**Status:** Accepted

## Context
The system spans networking, media, cryptography and three operating systems. Many choices
(transport, identity, wire format) are expensive to change later and easy to forget the reasons
for.

## Decision
Keep one short ADR per significant decision in `docs/adr/`. Number sequentially, never rewrite
history: a changed decision gets a new ADR that supersedes the old one. Decisions that rest on
a measurement link the benchmark and state what the benchmark did *not* cover.

## Consequences
Reviewers can see why, not just what. Cost: a few minutes per decision.
