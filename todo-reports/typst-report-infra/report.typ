#import "../template.typ": report, diagram

#show: report.with(title: "Typst report infrastructure", date: "2026-11-24")

= Context

Per `todos.md`, every completed todo item in this repository should produce a
short Typst report describing what was done, with diagrams where they help.
This report documents the infrastructure that makes that possible, and is
itself the first such report.

= What was added

- `todo-reports/<summary>/report.typ` is the convention: one folder per
  completed task, named after a short slug, containing a `report.typ` and any
  supporting files (data, generated plots, etc).
- `todo-reports/template.typ` is a tiny shared layout (`report`, `diagram`)
  used by every report so they stay visually consistent. It only uses
  Typst's built-in drawing primitives — no package-registry packages — so
  compilation never needs network access.
- `flake.nix` gained `pkgs.typst` in the dev shell, and a `build-reports` app
  (`nix run .#build-reports`) that finds every `todo-reports/**/report.typ`
  and compiles it to a sibling `.pdf`.

= Pipeline

#diagram(
  boxes: (("report.typ", 0, 0), ("typst compile", 1, 0), ("report.pdf", 2, 0)),
  arrows: ((0, 1), (1, 2)),
)

= Why this shape

The task list calls for a report per completed item, so the natural
granularity is one folder per task rather than one monolithic document — it
keeps `git log` / `git diff` scoped to the task that produced each report, and
lets a report be regenerated (or its PDF dropped from version control) without
touching the others. Keeping the template free of external Typst packages
means `nix run .#build-reports` is reproducible in the same spirit as the
rest of the flake — no registry fetch at build time.
