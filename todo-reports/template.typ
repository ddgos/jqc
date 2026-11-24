// Shared layout for todo-reports/<summary>/report.typ documents.
// Intentionally uses only typst's built-in drawing primitives (no package
// registry fetches) so `nix run .#build-reports` works offline.

#let report(title: "", date: none, body) = {
  set document(title: title)
  set page(margin: 2cm, numbering: "1")
  set text(font: "New Computer Modern", size: 11pt)
  set heading(numbering: "1.")

  align(center)[
    #text(size: 20pt, weight: "bold")[#title]
    #v(0.3cm)
    #text(size: 10pt, fill: gray)[#date]
  ]
  v(0.8cm)
  body
}

// A plain box-and-arrow diagram, e.g.:
//   #diagram(
//     boxes: (("lexer", 0, 0), ("parser", 1, 0), ("codegen", 2, 0)),
//     arrows: ((0, 1), (1, 2)),  // indices into `boxes`
//   )
#let diagram(boxes: (), arrows: (), cell: 2.6cm) = {
  box(
    width: 100%,
    height: (calc.max(1, boxes.map(b => b.at(2)).fold(0, calc.max)) + 1) * cell,
    stroke: none,
  )[
    #place(top + left, dx: 0cm, dy: 0cm)[
      #for (name, col, row) in boxes {
        place(top + left, dx: col * cell, dy: row * cell)[
          #box(
            width: cell * 0.85,
            height: cell * 0.5,
            stroke: 0.8pt + black,
            radius: 3pt,
            align(center + horizon, text(size: 9pt, name)),
          )
        ]
      }
      #for (from, to) in arrows {
        let (_, c1, r1) = boxes.at(from)
        let (_, c2, r2) = boxes.at(to)
        place(top + left, dx: c1 * cell + cell * 0.85, dy: r1 * cell + cell * 0.25)[
          #line(
            length: (c2 - c1) * cell - cell * 0.85,
            angle: 0deg,
            stroke: 0.8pt + black,
          )
        ]
      }
    ]
  ]
}
