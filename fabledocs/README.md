# fabledocs — index

*Index last updated 2026-09-23.*

This folder is the record of the v3.1 work: the implementation pass of
2026-09-18, the reviews that followed, and the release evidence being
gathered. It is a historical record. Current product behavior is specified in
[`docs/SPEC.md`](../docs/SPEC.md) and described for users in
[`USER-GUIDE.md`](USER-GUIDE.md); what has been validated on real hardware is
recorded only in the results ledger of
[`docs/RELEASE_CHECKLIST.md`](../docs/RELEASE_CHECKLIST.md).

## Status

As of 2026-09-23:

- **Implemented, not validated on hardware.** The v3.1 features (call
  profiles, dock to camera, focus mode, the performance work) are in the
  working tree. The change set was uncommitted when the implementation pass
  ended on 2026-09-18; that statement describes that date, not the
  repository permanently.
- **Review actions implemented, not validated on hardware.** The agreed
  actions R1–R7 from
  [the 2026-09-19 review](PROJECT-REVIEW-2026-09-19.md#agreed-actions-maintainer-2026-09-19)
  are implemented in the working tree, uncommitted, with their automated
  tests passing:
  - R1 session outcomes (the UI adopts early answer text and reconciles how
    each answer ended; a crash guard settles a failed answer) and R2
    cloud-stream completion (an answer counts as finished only when the
    provider says so; answers carry *incomplete* / *cut short* / *stopped*
    tags).
  - R3 Settings save lock, R5 settings revisions (the "Settings changed
    elsewhere — reload" flow) and damaged-settings-file quarantine.
  - R4 local prompt budget (a live byte readout in Settings and refusal
    before recording or sending).
  - R6 release checklist, results ledger and CI workflow
    (`.github/workflows/ci.yml`).
  - R7 screen-sharing wording; the app now also verifies Windows capture
    exclusion at launch and refuses to start without it.

  A finding is closed only when its acceptance checks are demonstrated on
  real hardware, not when a fix is written. None has been yet.
- **Release evidence.** The results ledger in
  [`docs/RELEASE_CHECKLIST.md`](../docs/RELEASE_CHECKLIST.md) has no recorded
  run yet, and the list of tested sharing configurations
  ([TROUBLESHOOTING.md](../docs/TROUBLESHOOTING.md#tested-sharing-configurations))
  is empty until a run is recorded.

## Current documents

- [`USER-GUIDE.md`](USER-GUIDE.md) — install, build, first-time setup, using
  the app on a call, profiles, window placement, troubleshooting, and free
  local voice from an installed app. Revised 2026-09-23.

## Reviews

In date order. Each later review reads the earlier ones.

1. [`REPORT.md`](REPORT.md) (2026-09-18) — the implementation report for the
   v3.1 pass: what changed, why, how it was verified, and what could not be
   verified on that machine. Its verification results describe that pass's
   scope only; they are not carried forward as release evidence.
2. [`PROJECT-REVIEW-2026-09-19.md`](PROJECT-REVIEW-2026-09-19.md)
   (2026-09-19) — independent project review with findings R1–R7, annotated
   with the maintainer's responses and ending in the agreed action list.
3. [`FINAL-REVIEW-AND-ADDITIONS-2026-09-19.md`](FINAL-REVIEW-AND-ADDITIONS-2026-09-19.md)
   (2026-09-19) — review of those responses: the final decision per finding,
   refinements required before implementing, and recommended additions.
4. [`READINESS-REPORT-2026-09-22.md`](READINESS-REPORT-2026-09-22.md)
   (2026-09-22) — readiness report on the R1–R7 work; to be added today.

## Historical material

Kept for provenance and decision history. Each file carries a banner saying
so; do not read it as a description of the current app.

- [`AUDIT.md`](AUDIT.md) (2026-09-18) — the audit that preceded v3.1, with
  the build / defer / reject decision for each finding.
- [`DESIGN.md`](DESIGN.md) (2026-09-18) — the implementation contract for
  that pass (IPC shapes, disk format, prompt additions, layout, work
  packages, verification matrix, non-goals).
- [`design/study-eye-level-answer.md`](design/study-eye-level-answer.md),
  [`design/study-call-profiles.md`](design/study-call-profiles.md) — archived
  design proposals.
- [`design/audit-findings-raw.md`](design/audit-findings-raw.md) — raw
  audit output, partly truncated; `AUDIT.md` holds the decisions.
