;; Plugin that imports a capability-gated host fn (`ainb_sessions_list`).
;; The capability gate test loads this fixture twice:
;;   1. Without the `read_sessions` capability — instantiation fails because
;;      the import is unresolved (the "refused statically" property).
;;   2. With `read_sessions = true` — instantiation succeeds; `_init` runs.
(module
  (import "env" "ainb_sessions_list" (func $list (param i32 i32) (result i32)))
  (memory (export "memory") 1)

  (func (export "_init") (result i32)
    i32.const 0
    i32.const 0
    call $list
    drop
    i32.const 0)

  (func (export "_shutdown")
    nop))
