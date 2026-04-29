mod expr;
mod solver;

pub use expr::*;
pub(crate) use solver::SymbolicSolver;

use crate::event_label::{ConstraintEval, ConstraintKind, SymbolicVar};
use crate::runtime::execution::ExecutionState;

pub fn fresh_int() -> SymExpr {
    ExecutionState::with(|s| {
        let pos = s.next_pos();
        let id = s.must.borrow_mut().next_symbolic_var_id();
        s.must
            .borrow_mut()
            .handle_symbolic_var(SymbolicVar::new(pos, id, SymSort::Int));
        SymExpr::Var {
            id,
            sort: SymSort::Int,
        }
    })
}

pub fn fresh_bool() -> SymExpr {
    ExecutionState::with(|s| {
        let pos = s.next_pos();
        let id = s.must.borrow_mut().next_symbolic_var_id();
        s.must
            .borrow_mut()
            .handle_symbolic_var(SymbolicVar::new(pos, id, SymSort::Bool));
        SymExpr::Var {
            id,
            sort: SymSort::Bool,
        }
    })
}

pub fn eval(sym_expr: SymExpr) -> bool {
    ExecutionState::with(|s| {
        let pos = s.next_pos();
        let lab = ConstraintEval::new(pos, sym_expr, ConstraintKind::Branch, true);
        s.must.borrow_mut().handle_constraint_eval(lab)
    })
}

pub fn assume(sym_expr: SymExpr) {
    let ok = eval(sym_expr);
    crate::assume_impl(ok, None);
}

pub fn assert(sym_expr: SymExpr) {
    let ok = eval(sym_expr);
    crate::assert(ok);
}
