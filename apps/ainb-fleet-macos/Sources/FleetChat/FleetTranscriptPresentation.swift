import Foundation

// The Swift half of the ACP transcript taxonomy, in pure display terms so the
// tests can hold it without a window.
//
// This is a PORT of `AcpClassifier` in
// `ainb-tui/crates/ainb-hangar-proto/src/transcript.rs`, arm for arm, and the
// point of the port is that the notch and the terminal client read one
// transcript the same way. An operator watching both surfaces must not have to
// learn two vocabularies for one tool call, so every string this file emits is
// the string the Rust side emits: the same double spaces, the same `[error]`
// suffix, the same `· <subtype> · <duration>` run-status line, the same
// `plan · <status> · <content>` checklist row.
//
// The deliberate divergences, all forced by the representation rather than
// chosen, are named where they occur: object key ORDER in a compact JSON
// rendering (a Swift dictionary has no insertion order), the codex and
// stream-json arms (this client never reads a process executor's transcript),
// and `acp_row_text` (its only caller is the daemon's PR-URL scanner).
//
// The caps are NOT among them. They count the same unit the Rust counts, down
// to Unicode scalars rather than Swift `Character`s; see `truncateChars` for
// why that distinction is the difference between a bound and no bound.
//
// There is no scrub in this file, and that is not an omission. The Rust
// classifier scrubs before it cuts (#1187), because a cut through a credential
// leaves a prefix no shape matches. Here every payload arrives as a
// `FleetTranscriptChunk` from `fleet/transcript_list` or
// `fleet/transcript_event`, and the daemon scrubs each string value of that
// payload before the chunk leaves it (`transcript_chunk_wire`, #1199), so every
// cut below runs on text that is already scrubbed. A new input that does not
// come through those two methods needs its own scrub before any cap applies.
//
// Every `switch` below is exhaustive with NO `default`, per the rule
// `FleetChatPresentation.swift` states: a new lane or a new JSON shape must
// fail to COMPILE rather than fall through to whichever arm was written last.

/// Clip a one-line summary / snippet to this many display chars (char-safe).
///
/// `SUMMARY_MAX` in the Rust.
private let transcriptSummaryMax = 84

/// Hard ceiling on a single classified body, in display CHARS not bytes.
///
/// `BODY_MAX` in the Rust, and generous for the same reason: this is a leak
/// backstop, not a display width. An adapter is free to emit a megabyte of
/// prose or a base64 blob in one chunk, and every classified body here is held
/// in a `@Published` surface the whole notch observes, so an uncapped body is
/// an unbounded allocation on the render path.
///
/// The UNIT is load-bearing and it is why `truncateChars` exists. Adapter prose
/// carries non-ASCII routinely, so cutting at a byte index would split a code
/// point; the Rust side pins exactly that
/// (`the_body_cap_never_splits_a_multibyte_char`) and so does this one. The
/// unit is Unicode SCALARS, matching the Rust's `chars()`, because a Swift
/// `Character` is a grapheme cluster of unbounded length and a cap counted in
/// those would not bound anything at all.
private let transcriptBodyMax = 8192

/// Ceiling on the rows ONE chunk may classify into.
///
/// `ENTRIES_PER_LINE_MAX` in the Rust. A single chunk carrying a multi-line
/// text block yields one row per line, so the per-body cap above bounds each
/// row without bounding their count.
private let transcriptEntriesPerChunkMax = 512

/// Most tool calls remembered while awaiting their update.
///
/// `MAX_PENDING_TOOLS` in the Rust, and the same backstop rather than a working
/// limit: an adapter issues a handful per turn.
private let transcriptPendingToolsMax = 256

/// How many classified rows the surface keeps.
///
/// A CLIENT display choice, not a daemon constant, which is why it does not
/// live beside `fleetTranscriptListMax` in the wire file. One page is up to
/// `fleetTranscriptListMax` chunks and one chunk can classify into
/// `transcriptEntriesPerChunkMax` rows, so a page alone has a worst case of
/// fifty thousand rows before a single live chunk arrives. This is the ceiling
/// that makes the pane's cost a function of the ceiling instead of a function
/// of how chatty the adapter is.
///
/// The OLDEST rows go, which is what a fresh page of a busier transcript would
/// have shown anyway: this is a tail view of a running session.
let fleetTranscriptRowMax = 500

/// The five-lane transcript taxonomy, mirroring the Rust `MessageKind`.
///
/// NOT a wire enum: the daemon sends chunks, this client derives the lane, so
/// there is no token to be tolerant about and no `unknown` arm to add. A chunk
/// type the taxonomy does not carry yields NO row rather than an unknown one.
enum FleetTranscriptLane: String, Equatable, CaseIterable, Sendable {
    /// Adapter prose.
    case agent
    /// Adapter reasoning.
    case thinking
    /// A tool invocation.
    case toolCall
    /// A tool's result.
    case toolResult
    /// An error line.
    case error
}

/// One classified line, before it is anchored to the chunk it came from.
///
/// A named struct rather than the Rust's `(MessageKind, String)` tuple, because
/// a tuple has no key paths and the tests read these by lane and by body.
struct FleetTranscriptEntry: Equatable, Sendable {
    let lane: FleetTranscriptLane
    let body: String
}

/// One classified transcript row, ready to paint.
struct FleetTranscriptRow: Equatable, Identifiable, Sendable {
    /// `<ingest order>.<row index within the chunk>`.
    ///
    /// Derived rather than the chunk's `event_id`, because one chunk classifies
    /// into MANY rows and `ForEach` needs each of them to be distinct. Anchored
    /// to the commit cursor rather than to a UUID so the same chunk classified
    /// twice produces the same ids, which is what makes a replayed chunk a
    /// no-op instead of a duplicated tool call.
    let id: String
    /// The commit-ordered cursor of the chunk this row came from.
    let ingestOrder: Int64
    let lane: FleetTranscriptLane
    let body: String
}

/// The transcript state that SURVIVES a safety-net page.
///
/// Grouped so that "is this field carried across a page, or rebuilt by it" is
/// answered by which type a field is declared in, rather than by remembering to
/// edit a copy function. See `FleetChatSurface.transcriptState`.
///
/// The classifier belongs in here rather than beside it: it holds the pending
/// tool-title map, so a page that kept the rows but dropped the classifier
/// would re-render the next tool result under the unnamed `tool` form. Rows and
/// the state that produced them travel together or not at all.
struct FleetTranscriptState: Equatable, Sendable {
    /// The classified rows on screen, oldest first.
    var rows: [FleetTranscriptRow] = []
    /// The highest `ingest_order` folded, and the transcript's dedup key.
    ///
    /// A CURSOR rather than a set of ids because the classifier is stateful:
    /// re-classifying a chunk already held would consume the pending tool title
    /// its first pass consumed, and re-render the result under the unnamed
    /// form. A chunk at or below this is dropped BEFORE the taxonomy sees it,
    /// the same "strictly after" rule the daemon's forwarder pages by.
    ///
    /// Also what a reconnect resumes the live stream FROM, so the rows
    /// committed during an outage are replayed rather than skipped.
    var cursor: Int64?
    /// The taxonomy state that produced `rows`, carried so the live fold
    /// continues where the page left off.
    var classifier = AcpTranscriptClassifier()
    /// Whether the daemon's tail read left older rows behind.
    ///
    /// A SEAM, not an error, which is why it is not `transcriptDetail`: the
    /// rows shown are real as far as they go, and the pane says where they
    /// start rather than claiming the run began there. Carried, because it
    /// describes the rows it is carried with.
    var truncated = false
}

/// Display mappings for the transcript lanes, in ONE place.
///
/// Same reasoning as `FleetChatLabels`: a mapping written twice is a mapping
/// that can disagree with itself.
enum FleetTranscriptLabels {
    /// The lane's on-screen tag. The terminal client paints a glyph per lane
    /// (`▌ * → ← !`); a menu-bar pane has room for the word, and the word is
    /// what survives VoiceOver.
    static func lane(_ lane: FleetTranscriptLane) -> String {
        switch lane {
        case .agent: "REPLY"
        case .thinking: "THINKING"
        case .toolCall: "TOOL"
        case .toolResult: "RESULT"
        case .error: "ERROR"
        }
    }

    /// Spoken lane, so a VoiceOver user gets the distinction a sighted one gets
    /// from the colour.
    static func accessibilityLane(_ lane: FleetTranscriptLane) -> String {
        switch lane {
        case .agent: "Agent reply"
        case .thinking: "Agent thinking"
        case .toolCall: "Tool call"
        case .toolResult: "Tool result"
        case .error: "Error"
        }
    }

    /// Whether the lane is drawn with the loud styling. Errors only: a
    /// transcript that shouted at every tool call would not distinguish the one
    /// line an operator has to act on.
    static func laneIsLoud(_ lane: FleetTranscriptLane) -> Bool {
        switch lane {
        case .error: true
        case .agent, .thinking, .toolCall, .toolResult: false
        }
    }
}

/// The running classification state for ONE ACP session's transcript.
///
/// Stateful across chunks for exactly one reason, the same one the Rust states:
/// a `tool_call_update` carries only the `toolCallId`, so it can name its tool
/// only because the earlier `tool_call` chunk's `title` was remembered.
/// Everything else about a chunk is self-describing.
///
/// A VALUE type, unlike the Rust's `&mut self` classifier, and that is what
/// makes it safe to hang off `FleetChatSurface`: a page builds a fresh
/// classifier, runs its own chunks through it, and the live fold continues on
/// the copy that page published. A page the store DISOWNS takes its classifier
/// with it instead of leaving the live stream reading half-consumed tool
/// titles, which is what a shared reference-type classifier would have done.
///
/// `Equatable` includes the pending titles on purpose: two surfaces with
/// identical rows but different pending tool calls will classify the NEXT chunk
/// differently, so they are not the same state and must not compare equal.
struct AcpTranscriptClassifier: Equatable, Sendable {
    /// `toolCallId` → the tool's human title, so a later update can name it.
    private var toolTitles: [String: String] = [:]

    init() {}

    /// Classify one chunk into zero or more rows, in commit order.
    ///
    /// Total: a chunk type this taxonomy does not carry, and a known type with
    /// missing fields, both yield what they can (usually nothing) rather than
    /// failing. Feed one session's chunks through one classifier, oldest first.
    mutating func rows(for chunk: FleetTranscriptChunk) -> [FleetTranscriptRow] {
        classify(eventType: chunk.eventType, payload: chunk.payload)
            .enumerated()
            .map { index, entry in
                FleetTranscriptRow(
                    id: "\(chunk.ingestOrder).\(index)",
                    ingestOrder: chunk.ingestOrder,
                    lane: entry.lane,
                    body: entry.body
                )
            }
    }

    /// The Rust `AcpClassifier::classify_value`, arm for arm.
    ///
    /// Deliberately silent on the chunk types the Rust is silent on, and for
    /// the reason it gives: `acp.usage` is the run's token accounting and
    /// belongs on a status line, `acp.user_message` is the adapter echoing back
    /// a prompt that is the task's brief rather than its output, and
    /// `acp.turn_started` / `acp.context_rebuilt` are session bookkeeping.
    mutating func classify(
        eventType: String,
        payload: JSONValue
    ) -> [FleetTranscriptEntry] {
        var out: [FleetTranscriptEntry] = []
        switch eventType {
        case "acp.message":
            Self.foldText(payload, lane: .agent, into: &out)
        case "acp.thought":
            Self.foldText(payload, lane: .thinking, into: &out)
        case "acp.tool_call":
            foldTool(payload, into: &out)
        case "acp.plan":
            Self.foldPlan(payload, into: &out)
        case "acp.permission":
            Self.foldPermission(payload, into: &out)
        case "acp.transcript_truncated":
            Self.foldTruncated(payload, into: &out)
        case "acp.turn_completed", "acp.turn_failed", "acp.turn_interrupted":
            Self.foldTurnEnd(eventType: eventType, payload: payload, into: &out)
        // KEPT, and it is not the `default` this codebase bans: that rule is
        // about switches over a WIRE ENUM this build owns, where a new variant
        // must fail to compile. `event_type` is an open string the daemon and
        // its adapters choose, so an unknown one is a taxonomy this build does
        // not carry, and the contract is to skip it rather than mis-render it.
        default:
            break
        }
        return Self.capped(out)
    }

    /// A `tool_call` / `tool_call_update` chunk.
    ///
    /// The initial call is the tool-call lane (`<title>  <compact rawInput>`);
    /// an update that carries output or reached a terminal status is the
    /// result lane, and the ERROR lane when it failed. An update that only
    /// moves `pending → in_progress` is not a transcript row at all: every
    /// adapter emits one per call and it says nothing an operator can use.
    private mutating func foldTool(
        _ payload: JSONValue,
        into out: inout [FleetTranscriptEntry]
    ) {
        let id = payload.value("toolCallId")?.stringValue ?? ""
        let title = payload.value("title")?.stringValue
        let status = payload.value("status")?.stringValue ?? ""
        let isCall = payload.value("sessionUpdate")?.stringValue == "tool_call"

        if isCall {
            let name = title ?? "tool"
            let summary = Self.compactInput(payload.value("rawInput"))
            out.append(FleetTranscriptEntry(lane: .toolCall, body: summary.isEmpty ? name : "\(name)  \(summary)"))
            if !id.isEmpty {
                // The Rust's backstop, kept verbatim: past this many calls
                // awaiting an update, forget them ALL rather than track age. A
                // cancelled turn would otherwise pin its entry forever in a
                // classifier that lives as long as the pane. Raise the cap
                // before reaching for an LRU.
                if toolTitles.count >= transcriptPendingToolsMax {
                    toolTitles.removeAll()
                }
                toolTitles[id] = name
            }
        }

        let snippet = Self.toolOutputText(payload)
            .map { truncateChars(oneLine($0), transcriptSummaryMax) } ?? ""
        let terminal = status == "completed" || status == "failed"
        if snippet.isEmpty, !terminal { return }
        // An initial `tool_call` that ALREADY carries its output names itself;
        // a later update resolves the name from the call it belongs to, and
        // degrades to the unnamed form when that call fell outside the page.
        let name = title ?? toolTitles[id] ?? "tool"
        if status == "failed" {
            toolTitles.removeValue(forKey: id)
            out.append(FleetTranscriptEntry(lane: .error, body: snippet.isEmpty ? "\(name)  [error]" : "\(name)  \(snippet)  [error]"))
            return
        }
        if terminal {
            toolTitles.removeValue(forKey: id)
        }
        out.append(FleetTranscriptEntry(lane: .toolResult, body: snippet.isEmpty ? "\(name)  (\(status))" : "\(name)  \(snippet)"))
    }

    /// A coalesced text chunk (`{kind, text, coalescedDeltas}`), one row per
    /// line so a multi-line reply never overflows a render row.
    ///
    /// The same `event_type` also carries the reducer's NON-text shape (an
    /// image, audio, or embedded resource block, `text` empty and the verbatim
    /// update under `block`). That one has no text to show, so it is NAMED
    /// rather than dropped: a transcript that silently omits a row reads as a
    /// complete one.
    private static func foldText(
        _ payload: JSONValue,
        lane: FleetTranscriptLane,
        into out: inout [FleetTranscriptEntry]
    ) {
        let text = payload.value("text")?.stringValue ?? ""
        if !text.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty {
            pushLines(out: &out, lane: lane, text: text)
            return
        }
        if let block = payload.value("block") {
            let contentType = block.value("content")?.value("type")?.stringValue ?? "non-text"
            out.append(FleetTranscriptEntry(lane: lane, body: "· \(contentType) content"))
        }
    }

    /// A `plan` update: one tool-call row per entry, so the agent's plan reads
    /// as the checklist it is rather than as a JSON blob on one row.
    private static func foldPlan(
        _ payload: JSONValue,
        into out: inout [FleetTranscriptEntry]
    ) {
        guard let entries = payload.value("entries")?.arrayValue else { return }
        for entry in entries {
            let status = entry.value("status")?.stringValue ?? "pending"
            let content = entry.value("content")?.stringValue ?? ""
            out.append(FleetTranscriptEntry(
                lane: .toolCall,
                body: truncateChars(oneLine("plan · \(status) · \(content)"), transcriptSummaryMax)
            ))
        }
    }

    /// A parked permission ask, named for the tool it is gating.
    ///
    /// The payload is the daemon's own approval envelope (`acp_pool`'s
    /// `raise_permission`), not an adapter update, so the tool sits under
    /// `toolCall`.
    ///
    /// NOT a guardrail confirm card, and the two must never be merged in this
    /// pane: a confirm card is answered through `fleet/confirm_answer`, while
    /// this is an ACP permission answered as an attention row through
    /// `fleet/action`. This row is a READOUT of one, never a control.
    private static func foldPermission(
        _ payload: JSONValue,
        into out: inout [FleetTranscriptEntry]
    ) {
        let call = payload.value("toolCall")
        let name = call?.value("title")?.stringValue
            ?? call?.value("toolCallId")?.stringValue
            ?? "tool"
        out.append(FleetTranscriptEntry(lane: .toolCall, body: "[approval] \(name)"))
    }

    /// The writer's own "I threw rows away" marker, in the error lane.
    ///
    /// The one row the transcript MUST show: a silently short transcript reads
    /// as a complete one, which is the whole reason the store writer mints this
    /// instead of just dropping.
    private static func foldTruncated(
        _ payload: JSONValue,
        into out: inout [FleetTranscriptEntry]
    ) {
        let dropped = payload.value("droppedRows")?.int64Value ?? 0
        out.append(FleetTranscriptEntry(lane: .error, body: "· transcript truncated · \(dropped) rows dropped"))
    }

    /// A turn's closing marker as the run-status line the process executor's
    /// `result` line renders (`· <subtype> · <duration>`), so both executors
    /// close the same way.
    private static func foldTurnEnd(
        eventType: String,
        payload: JSONValue,
        into out: inout [FleetTranscriptEntry]
    ) {
        let subtype = eventType.hasPrefix("acp.") ? String(eventType.dropFirst("acp.".count)) : eventType
        var status = "· \(subtype)"
        if let ms = payload.value("durationMs")?.int64Value {
            status += " · \(formatDuration(ms))"
        }
        if let cause = payload.value("cause")?.stringValue {
            status += " · \(cause)"
        }
        out.append(FleetTranscriptEntry(
            lane: eventType == "acp.turn_completed" ? .toolResult : .error,
            body: status
        ))
    }

    /// The displayable output of a tool-call update: its `content` blocks'
    /// text, else its `rawOutput`.
    ///
    /// `content` is an array of `{type:"content"|"diff"|"terminal", …}` where
    /// the `content` flavour wraps a `{type:"text",text:…}` block. Only TEXT is
    /// pulled out: a diff or an embedded terminal contributes NOTHING rather
    /// than a JSON blob rendered as if it were the tool's output. The bare
    /// `{type:"text",…}` fallback is for an adapter that skips the wrapper, and
    /// the fallback fires only when the wrapped path is ABSENT, never when it
    /// is present but not a string.
    ///
    /// Blocks join on a NEWLINE, not a space, matching the Rust: the display
    /// path re-flattens to one line anyway, but the separator is what stops a
    /// whole-line-anchored scanner losing a URL an adapter split across two
    /// blocks.
    private static func toolOutputText(_ payload: JSONValue) -> String? {
        if let blocks = payload.value("content")?.arrayValue {
            let joined = blocks.compactMap { block -> String? in
                let wrapped = block.value("content")?.value("text")
                return (wrapped ?? block.value("text"))?.stringValue
            }.joined(separator: "\n")
            if !joined.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty {
                return joined
            }
        }
        return payload.value("rawOutput").flatMap(valueText)
    }

    /// The ONE gate every classified row passes through.
    ///
    /// It lives here rather than at the dozen append sites because a tool
    /// `name`, a `subtype` and a tool `title` are adapter strings too, and
    /// capping only the prose in `pushLines` left those uncapped.
    private static func capped(
        _ out: [FleetTranscriptEntry]
    ) -> [FleetTranscriptEntry] {
        var out = out
        if out.count > transcriptEntriesPerChunkMax {
            // Say so rather than dropping silently, the way the body cap
            // appends its ellipsis: a six-hundred-line block otherwise loses
            // eighty-eight lines with no sign at all. The COUNT is the part an
            // off-by-one ships silently, so the Rust pins it and so does this.
            let kept = transcriptEntriesPerChunkMax - 1
            let dropped = out.count - kept
            out.removeLast(dropped)
            out.append(FleetTranscriptEntry(lane: .toolResult, body: "… \(dropped) more lines"))
        }
        return out.map { entry in
            entry.body.unicodeScalars.count > transcriptBodyMax
                ? FleetTranscriptEntry(lane: entry.lane, body: truncateChars(entry.body, transcriptBodyMax))
                : entry
        }
    }

    /// Split `text` into non-empty trimmed lines and append one row per line,
    /// so a multi-line block never overflows a single render row.
    ///
    /// Rust's `str::lines()` semantics, not Swift's `components(separatedBy:)`:
    /// a single trailing newline yields no extra line, and a trailing carriage
    /// return is stripped.
    private static func pushLines(
        out: inout [FleetTranscriptEntry],
        lane: FleetTranscriptLane,
        text: String
    ) {
        for line in rustLines(text) {
            let trimmed = trimTrailingWhitespace(line)
            if trimmed.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty { continue }
            out.append(FleetTranscriptEntry(lane: lane, body: trimmed))
        }
    }

    /// A compact one-line summary of a tool call's `rawInput`: the most telling
    /// field for the common tools (a shell command, a file path, a query), else
    /// a truncated flat rendering, so a tool call reads at a glance without its
    /// full JSON payload.
    private static func compactInput(_ input: JSONValue?) -> String {
        guard let object = input?.objectValue else {
            return input.map(compactValue) ?? ""
        }
        for key in ["command", "file_path", "path", "pattern", "query", "url", "prompt"] {
            if let text = object[key].flatMap(valueText) {
                return truncateChars(oneLine(text), transcriptSummaryMax)
            }
        }
        // No telling field: a flat, truncated key=val rendering. Sorted, which
        // is what walking a `serde_json::Map` gives too, since that map is a
        // `BTreeMap` unless `preserve_order` is on and it is not. Neither side
        // renders the adapter's own order; a Swift dictionary does not keep one.
        let flat = object.keys.sorted()
            .map { "\($0)=\(oneLine(compactValue(object[$0] ?? .null)))" }
            .joined(separator: " ")
        return truncateChars(flat, transcriptSummaryMax)
    }

    /// Read a JSON value as display text: a string as-is, else its compact
    /// JSON. A tool result's `content` is often an array of
    /// `{type:"text", text:…}` blocks, so their text is joined.
    private static func valueText(_ value: JSONValue) -> String? {
        switch value {
        case let .string(text):
            return text
        case let .array(items):
            let joined = items.compactMap { item -> String? in
                item.value("text")?.stringValue ?? item.stringValue
            }.joined(separator: " ")
            return joined.isEmpty ? nil : joined
        case .null:
            return nil
        case .bool, .number, .object:
            return compactJSON(value)
        }
    }

    /// A short flat rendering of any JSON value for a compact input summary.
    private static func compactValue(_ value: JSONValue) -> String {
        switch value {
        case let .string(text): text
        case .null, .bool, .number, .array, .object: compactJSON(value)
        }
    }
}

// MARK: - Rendering primitives
//
// Free functions rather than methods because none of them touch classifier
// state, and every one has a Rust twin whose behaviour is pinned by a test on
// both sides.

/// Collapse every whitespace run (newlines included) to single spaces, trimmed.
/// Keeps a summary on one render row.
private func oneLine(_ text: String) -> String {
    text.split(whereSeparator: { $0.isWhitespace }).joined(separator: " ")
}

/// Format a duration in milliseconds as `Nms` / `N.Ns` / `NmNs`.
///
/// A negative delta (clock skew between two chunks) reads `0ms` rather than a
/// nonsense value.
private func formatDuration(_ milliseconds: Int64) -> String {
    let ms = max(0, milliseconds)
    if ms < 1000 { return "\(ms)ms" }
    if ms < 60_000 { return String(format: "%.1fs", Double(ms) / 1000.0) }
    return "\(ms / 60_000)m\((ms % 60_000) / 1000)s"
}

/// Truncate to `max` Unicode SCALARS with a trailing ellipsis on overflow.
///
/// Scalars, matching the Rust's `chars()`, and NOT Swift `Character`s. The two
/// are not interchangeable and picking the wrong one defeats the cap: a
/// grapheme cluster has no length limit, so one combining sequence of a million
/// scalars counts as a single `Character` and would sail through a
/// grapheme-counted cap untouched. That is the opposite of what a leak backstop
/// is for, and it is reachable by any adapter that emits combining marks.
///
/// Slicing scalars is still safe in the way byte slicing is not: a scalar
/// boundary always exists, so this cannot panic or split a code point the way
/// the UTF-8 truncation trap does.
private func truncateChars(_ text: String, _ max: Int) -> String {
    let scalars = text.unicodeScalars
    guard scalars.count > max else { return text }
    return String(String.UnicodeScalarView(scalars.prefix(Swift.max(0, max - 1)))) + "…"
}

/// The wording the daemon's own board timeline uses for the same seam, so an
/// operator reading both surfaces reads one sentence.
///
/// Deliberately the ERROR lane and deliberately FIRST: it describes where the
/// rows below it begin, and a marker under them would read as the end of the
/// run rather than the start of what was kept.
let fleetTranscriptTruncationRow = FleetTranscriptRow(
    id: "truncated",
    ingestOrder: 0,
    lane: .error,
    body: "· transcript truncated · older lines not shown"
)

/// `str::lines()` semantics: split on newline, drop the ONE empty piece a
/// trailing newline produces, strip a trailing carriage return from each line.
private func rustLines(_ text: String) -> [String] {
    var parts = text.components(separatedBy: "\n")
    if parts.last == "" { parts.removeLast() }
    return parts.map { $0.hasSuffix("\r") ? String($0.dropLast()) : $0 }
}

/// Rust's `str::trim_end`, which trims trailing whitespace only.
private func trimTrailingWhitespace(_ text: String) -> String {
    var text = text
    while let last = text.last, last.isWhitespace { text.removeLast() }
    return text
}

/// Compact JSON for one value, the way `serde_json`'s `to_string` renders it.
///
/// Written here rather than routed through `JSONEncoder` for two reasons: a
/// `Decimal` must render as the adapter wrote it rather than through a float
/// formatter, and a top-level fragment must be encodable at all.
///
/// The ONE divergence from the Rust is object key ORDER, which is sorted here.
/// It is not a choice: `JSONValue.object` is a Swift dictionary, so the
/// adapter's order is already gone by the time this function is reached. Sorted
/// is the only rendering that is at least stable across runs.
private func compactJSON(_ value: JSONValue) -> String {
    switch value {
    case .null:
        return "null"
    case let .bool(flag):
        return flag ? "true" : "false"
    case let .number(number):
        return number.description
    case let .string(text):
        return jsonQuoted(text)
    case let .array(items):
        return "[" + items.map(compactJSON).joined(separator: ",") + "]"
    case let .object(fields):
        let body = fields.keys.sorted()
            .map { "\(jsonQuoted($0)):\(compactJSON(fields[$0] ?? .null))" }
            .joined(separator: ",")
        return "{" + body + "}"
    }
}

/// One JSON string literal, escaped to `serde_json`'s rules.
private func jsonQuoted(_ text: String) -> String {
    var out = "\""
    for scalar in text.unicodeScalars {
        switch scalar {
        case "\"": out += "\\\""
        case "\\": out += "\\\\"
        case "\n": out += "\\n"
        case "\r": out += "\\r"
        case "\t": out += "\\t"
        default:
            if scalar.value < 0x20 {
                out += String(format: "\\u%04x", scalar.value)
            } else {
                out.unicodeScalars.append(scalar)
            }
        }
    }
    return out + "\""
}

extension JSONValue {
    /// The value as a whole number, the way `serde_json`'s `as_i64` reads one.
    ///
    /// A FRACTIONAL number is not a whole one and answers nil, matching the
    /// Rust rather than silently rounding: `durationMs: 1.5` is an adapter bug
    /// and a transcript that rendered it as `1ms` would hide it.
    var int64Value: Int64? {
        guard case let .number(number) = self else { return nil }
        var source = number
        var whole = Decimal()
        NSDecimalRound(&whole, &source, 0, .down)
        guard whole == number else { return nil }
        guard whole >= Decimal(Int64.min), whole <= Decimal(Int64.max) else { return nil }
        return NSDecimalNumber(decimal: whole).int64Value
    }
}
