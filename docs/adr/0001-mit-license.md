# ADR-0001: License: MIT

Uncompose Compare needs a license before any public work lands, and the choice is
effectively irreversible: once outside contributions exist, relicensing would require
every contributor's consent, and no CLA is planned that could grant it. We decided:
**MIT**, copyright Dominic Hanzely.

Compare is a member of the Uncompose family, and the family template is MIT (uncompose
ADR-0002). Matching it is the default; the decision was re-made on its own merits rather
than inherited blindly:

- The surrounding audio-tooling ecosystem is MIT. Matching it removes friction for
  embedding, wrapping, and packaging Compare, and for reusing its comparison-record schema
  in downstream tools.
- MIT matches the project identity: Compare is a tool musicians own and run locally, not a
  service they rent. A maximally permissive license is the licensing expression of that.
- Compare ships no bundled model weights or other license-encumbered payload, so nothing
  constrains the choice of code license.

## Considered options

- **AGPL-3.0** — would force a third party offering Compare as a closed hosted service to
  share their changes. Considered and consciously declined: Compare's value is the
  local-first listening workflow, a hosted wrapper doesn't threaten it, and copyleft cuts
  against the ecosystem norm and adds adoption friction for the developers and packagers
  Compare wants as contributors.
- **Apache-2.0** — the explicit patent grant is the only material difference from MIT. Not
  worth diverging from the ecosystem's MIT norm for a project with no patent exposure, and
  it would split the family's license.

## Consequences

- Anyone may build closed or commercial products on Compare, including hosted services.
  Accepted, per the above.
- The license is locked in practice from the first outside contribution (no CLA, so no
  relicensing path). This ADR records that this was understood at decision time.
- The family stays single-license (MIT across uncompose, project, and compare), so code
  and schemas move between the repos without a licensing seam.
