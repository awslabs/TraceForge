use crate::symbolic::{SymExpr, SymSort};
use z3::ast::{Bool, Int};

pub(crate) struct SymbolicSolver {
    solver: z3::Solver,
}

impl SymbolicSolver {
    pub(crate) fn new() -> Self {
        Self {
            solver: z3::Solver::new(),
        }
    }

    pub(crate) fn assert(&mut self, expr: &SymExpr) {
        let b = self.compile_bool(expr);
        self.solver.assert(&b);
    }

    pub(crate) fn assert_not(&mut self, expr: &SymExpr) {
        let b = self.compile_bool(expr);
        self.solver.assert(&b.not());
    }

    pub(crate) fn reset(&mut self) {
        self.solver.reset();
    }

    pub(crate) fn sat_with(&self, expr: &SymExpr) -> bool {
        let b = self.compile_bool(expr);
        self.solver.push();
        self.solver.assert(&b);
        let is_sat = self.is_sat();
        self.solver.pop(1);
        is_sat
    }

    pub(crate) fn sat_with_not(&self, expr: &SymExpr) -> bool {
        let b = self.compile_bool(expr);
        self.solver.push();
        self.solver.assert(&b.not());
        let is_sat = self.is_sat();
        self.solver.pop(1);
        is_sat
    }

    pub(crate) fn is_sat(&self) -> bool {
        self.solver.check() == z3::SatResult::Sat
    }

    fn compile_bool(&self, expr: &SymExpr) -> Bool {
        match expr {
            SymExpr::Var {
                id,
                sort: SymSort::Bool,
            } => Bool::new_const(format!("sym_b_{}", id.0)),
            SymExpr::Bool(value) => Bool::from_bool(*value),
            SymExpr::Not(inner) => self.compile_bool(inner).not(),
            SymExpr::And(lhs, rhs) => {
                let lhs = self.compile_bool(lhs);
                let rhs = self.compile_bool(rhs);
                Bool::and(&[lhs, rhs])
            }
            SymExpr::Or(lhs, rhs) => {
                let lhs = self.compile_bool(lhs);
                let rhs = self.compile_bool(rhs);
                Bool::or(&[lhs, rhs])
            }
            SymExpr::Implies(lhs, rhs) => self.compile_bool(lhs).implies(self.compile_bool(rhs)),
            SymExpr::Eq(lhs, rhs) => match self.expr_sort(lhs) {
                SymSort::Bool => self.compile_bool(lhs).eq(self.compile_bool(rhs)),
                SymSort::Int => self.compile_int(lhs).eq(self.compile_int(rhs)),
            },
            SymExpr::Gt(lhs, rhs) => self.compile_int(lhs).gt(self.compile_int(rhs)),
            SymExpr::Ge(lhs, rhs) => self.compile_int(lhs).ge(self.compile_int(rhs)),
            SymExpr::Lt(lhs, rhs) => self.compile_int(lhs).lt(self.compile_int(rhs)),
            SymExpr::Le(lhs, rhs) => self.compile_int(lhs).le(self.compile_int(rhs)),
            other => panic!("expected symbolic bool expression, got {:?}", other),
        }
    }

    fn compile_int(&self, expr: &SymExpr) -> Int {
        match expr {
            SymExpr::Var {
                id,
                sort: SymSort::Int,
            } => Int::new_const(format!("sym_i_{}", id.0)),
            SymExpr::Int(value) => Int::from_i64(*value),
            SymExpr::Add(lhs, rhs) => {
                let lhs = self.compile_int(lhs);
                let rhs = self.compile_int(rhs);
                Int::add(&[lhs, rhs])
            }
            SymExpr::Sub(lhs, rhs) => {
                let lhs = self.compile_int(lhs);
                let rhs = self.compile_int(rhs);
                Int::sub(&[lhs, rhs])
            }
            SymExpr::Mul(lhs, rhs) => {
                let lhs = self.compile_int(lhs);
                let rhs = self.compile_int(rhs);
                Int::mul(&[lhs, rhs])
            }
            SymExpr::Div(lhs, rhs) => self.compile_int(lhs).div(self.compile_int(rhs)),
            SymExpr::Rem(lhs, rhs) => self.compile_int(lhs).rem(self.compile_int(rhs)),
            other => panic!("expected symbolic int expression, got {:?}", other),
        }
    }

    fn expr_sort(&self, expr: &SymExpr) -> SymSort {
        match expr {
            SymExpr::Var { sort, .. } => *sort,
            SymExpr::Bool(_)
            | SymExpr::Not(_)
            | SymExpr::And(_, _)
            | SymExpr::Or(_, _)
            | SymExpr::Implies(_, _)
            | SymExpr::Eq(_, _)
            | SymExpr::Gt(_, _)
            | SymExpr::Ge(_, _)
            | SymExpr::Lt(_, _)
            | SymExpr::Le(_, _) => SymSort::Bool,
            SymExpr::Int(_)
            | SymExpr::Add(_, _)
            | SymExpr::Sub(_, _)
            | SymExpr::Mul(_, _)
            | SymExpr::Div(_, _)
            | SymExpr::Rem(_, _) => SymSort::Int,
        }
    }
}
