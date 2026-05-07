;; Minimal "hello world" plugin used by the host loader test.
;;
;; Exports just enough surface for Phase 1:
;;   * `memory` — required so host fns can read/write strings.
;;   * `_init`  — returns 0 to signal success.
;;   * `_shutdown` — no-op.
;;
;; Imports the always-on baseline host fns. The host links these unconditionally,
;; so this fixture loads under any (or no) capability set.
(module
  (import "env" "ainb_log"          (func $log (param i32 i32 i32)))
  (import "env" "ainb_now_ms"       (func $now (result i64)))
  (import "env" "ainb_render_buffer"(func $render (param i32 i32 i32)))
  (memory (export "memory") 1)

  ;; Sentinel string at offset 0: "hi"
  (data (i32.const 0) "hi")

  (func (export "_init") (result i32)
    ;; Log "hi" at level=2 (info)
    i32.const 2
    i32.const 0
    i32.const 2
    call $log
    i32.const 0)

  (func (export "_shutdown")
    nop))
