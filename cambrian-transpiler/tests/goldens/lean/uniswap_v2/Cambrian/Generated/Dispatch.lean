/-
  Auto-generated dispatch table for dynamic-address sends.
  Each entry models `~> dest` / `var x = msg(args) ~> dest` where `dest`
  is an `address`-typed value the codegen couldn't pin to a concrete
  `(entity, identity)` at compile time. Wrappers return Except so CALL
  revert (where/throw / underfunded value) fail-closes like EVM.
-/

import Cambrian.Prelude
import Cambrian.Generated.World

set_option linter.unusedVariables false

namespace Cambrian.Generated.Dispatch

namespace ERC20
  /-- Dynamic dispatch for `ERC20.balanceOf` against an `Address<ERC20>` (Except; may revert). -/
  opaque balanceOf (w : Cambrian.Generated.World)
             (dest : Cambrian.Address)
             (value : Cambrian.U256)
             (ctx : Cambrian.MsgCtx) (owner : Cambrian.Address)
      : Except Cambrian.ThrowCode (Cambrian.Generated.World × Nat)
end ERC20

namespace ERC20
  /-- Dynamic dispatch for `ERC20.transfer` against an `Address<ERC20>` (Except; may revert). -/
  opaque transfer (w : Cambrian.Generated.World)
             (dest : Cambrian.Address)
             (value : Cambrian.U256)
             (ctx : Cambrian.MsgCtx) (to : Cambrian.Address) (amount : Nat)
      : Except Cambrian.ThrowCode (Cambrian.Generated.World)
end ERC20

end Cambrian.Generated.Dispatch
