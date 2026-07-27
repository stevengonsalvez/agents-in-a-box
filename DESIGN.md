# Fleet Command Cockpit Design System

## Theme

Dark terminal-native operator surface. Near-black blue-charcoal field, cool
white data ink, cobalt navigation, mint ready state, amber waiting state, and
coral error state. Color supports structure, never decoration.

## Tokens

| Role | Terminal color | Use |
| --- | --- | --- |
| Ink | `rgb(226, 232, 240)` | Primary text |
| Muted | `rgb(148, 163, 184)` | Labels, metadata |
| Cobalt | `rgb(96, 165, 250)` | Focus, dividers, navigation |
| Mint | `rgb(94, 234, 212)` | Ready, delivered, managed |
| Amber | `rgb(251, 191, 36)` | Ask, waiting, warnings |
| Coral | `rgb(251, 113, 133)` | Errors, destructive state |

## Layout

- Fleet roster owns left two-thirds at wide terminals.
- Detail owns right third and begins with selected-session state.
- Structured interviews open as a full-height workbench with a tab strip,
  active question, options, progress, and exact keyboard legend.
- At narrow widths, retain readable roster chrome and render workbench full
  width. Do not alter global sidebar or split behavior.

## Components

- Command bar: title, current filter, visible count, authoritative revision.
- Roster row: provider code, lifecycle, textual attention, mode, signal, link.
- Selected row: leading focus glyph and high-contrast session identity.
- Interview tabs: question number, short header, answered or pending marker.
- Answer option: radio or checkbox glyph, label, optional description.

## Interaction

- Existing browse keys stay unchanged.
- `Enter` opens an ASK and advances or submits the current interview answer.
- `Tab` and `Shift+Tab` move between interview questions after the workbench is
  open.
- `Space` toggles multi-select values. `o` enters free text for Other.
- `Esc` exits without sending.
