use crate::symbolic::{SymExpr, SymFunc, SymSort};
use z3::ast::{Ast, Bool, Dynamic, Int};
use z3::{FuncDecl, Sort, Symbol};

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
        self.compile_dynamic(expr)
            .as_bool()
            .unwrap_or_else(|| panic!("expected symbolic bool expression, got {:?}", expr))
    }

    fn compile_int(&self, expr: &SymExpr) -> Int {
        self.compile_dynamic(expr)
            .as_int()
            .unwrap_or_else(|| panic!("expected symbolic int expression, got {:?}", expr))
    }

    fn compile_dynamic(&self, expr: &SymExpr) -> Dynamic {
        match expr {
            SymExpr::Var {
                id,
                sort: SymSort::Bool,
            } => Dynamic::from_ast(&Bool::new_const(format!("sym_b_{}", id.0))),
            SymExpr::Var {
                id,
                sort: SymSort::Int,
            } => Dynamic::from_ast(&Int::new_const(format!("sym_i_{}", id.0))),
            SymExpr::Var {
                id,
                sort: sort @ SymSort::Uninterpreted(_),
            } => {
                let sort = self.compile_sort(sort);
                Dynamic::new_const(format!("sym_u_{}", id.0), &sort)
            }
            SymExpr::Bool(value) => Dynamic::from_ast(&Bool::from_bool(*value)),
            SymExpr::Int(value) => Dynamic::from_ast(&Int::from_i64(*value)),
            SymExpr::App { func, args } => self.compile_app(func, args),
            SymExpr::Not(inner) => Dynamic::from_ast(&self.compile_bool(inner).not()),
            SymExpr::And(lhs, rhs) => {
                let lhs = self.compile_bool(lhs);
                let rhs = self.compile_bool(rhs);
                Dynamic::from_ast(&Bool::and(&[lhs, rhs]))
            }
            SymExpr::Or(lhs, rhs) => {
                let lhs = self.compile_bool(lhs);
                let rhs = self.compile_bool(rhs);
                Dynamic::from_ast(&Bool::or(&[lhs, rhs]))
            }
            SymExpr::Implies(lhs, rhs) => {
                Dynamic::from_ast(&self.compile_bool(lhs).implies(self.compile_bool(rhs)))
            }
            SymExpr::Eq(lhs, rhs) => {
                self.expect_same_sort(lhs, rhs, "equality");
                let lhs = self.compile_dynamic(lhs);
                let rhs = self.compile_dynamic(rhs);
                Dynamic::from_ast(&lhs.eq(&rhs))
            }
            SymExpr::Gt(lhs, rhs) => {
                Dynamic::from_ast(&self.compile_int(lhs).gt(self.compile_int(rhs)))
            }
            SymExpr::Ge(lhs, rhs) => {
                Dynamic::from_ast(&self.compile_int(lhs).ge(self.compile_int(rhs)))
            }
            SymExpr::Lt(lhs, rhs) => {
                Dynamic::from_ast(&self.compile_int(lhs).lt(self.compile_int(rhs)))
            }
            SymExpr::Le(lhs, rhs) => {
                Dynamic::from_ast(&self.compile_int(lhs).le(self.compile_int(rhs)))
            }
            SymExpr::Add(lhs, rhs) => {
                self.expect_int_operands(lhs, rhs, "addition");
                let lhs = self.compile_int(lhs);
                let rhs = self.compile_int(rhs);
                Dynamic::from_ast(&Int::add(&[lhs, rhs]))
            }
            SymExpr::Sub(lhs, rhs) => {
                self.expect_int_operands(lhs, rhs, "subtraction");
                let lhs = self.compile_int(lhs);
                let rhs = self.compile_int(rhs);
                Dynamic::from_ast(&Int::sub(&[lhs, rhs]))
            }
            SymExpr::Mul(lhs, rhs) => {
                self.expect_int_operands(lhs, rhs, "multiplication");
                let lhs = self.compile_int(lhs);
                let rhs = self.compile_int(rhs);
                Dynamic::from_ast(&Int::mul(&[lhs, rhs]))
            }
            SymExpr::Div(lhs, rhs) => {
                self.expect_int_operands(lhs, rhs, "division");
                Dynamic::from_ast(&self.compile_int(lhs).div(self.compile_int(rhs)))
            }
            SymExpr::Rem(lhs, rhs) => {
                self.expect_int_operands(lhs, rhs, "remainder");
                Dynamic::from_ast(&self.compile_int(lhs).rem(self.compile_int(rhs)))
            }
        }
    }

    fn expr_sort(&self, expr: &SymExpr) -> SymSort {
        match expr {
            SymExpr::Var { sort, .. } => sort.clone(),
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
            SymExpr::App { func, args } => {
                self.validate_app(func, args);
                func.range().clone()
            }
        }
    }

    fn compile_sort(&self, sort: &SymSort) -> Sort {
        match sort {
            SymSort::Bool => Sort::bool(),
            SymSort::Int => Sort::int(),
            SymSort::Uninterpreted(name) => Sort::uninterpreted(Symbol::String(name.clone())),
        }
    }

    fn compile_func(&self, func: &SymFunc) -> FuncDecl {
        let domain = func
            .domain()
            .iter()
            .map(|sort| self.compile_sort(sort))
            .collect::<Vec<_>>();
        let domain_refs = domain.iter().collect::<Vec<_>>();
        let range = self.compile_sort(func.range());

        FuncDecl::new(func.name(), &domain_refs, &range)
    }

    fn compile_app(&self, func: &SymFunc, args: &[SymExpr]) -> Dynamic {
        self.validate_app(func, args);

        let decl = self.compile_func(func);
        let args = args
            .iter()
            .map(|arg| self.compile_dynamic(arg))
            .collect::<Vec<_>>();
        let arg_refs = args.iter().map(|arg| arg as &dyn Ast).collect::<Vec<_>>();

        decl.apply(&arg_refs)
    }

    fn validate_app(&self, func: &SymFunc, args: &[SymExpr]) {
        if func.domain().len() != args.len() {
            panic!(
                "uninterpreted function `{}` expects {} arguments, got {}",
                func.name(),
                func.domain().len(),
                args.len()
            );
        }

        for (index, (expected, actual)) in func.domain().iter().zip(args).enumerate() {
            let actual = self.expr_sort(actual);
            if expected != &actual {
                panic!(
                    "uninterpreted function `{}` argument {} expected sort {:?}, got {:?}",
                    func.name(),
                    index,
                    expected,
                    actual
                );
            }
        }
    }

    fn expect_same_sort(&self, lhs: &SymExpr, rhs: &SymExpr, op: &str) {
        let lhs_sort = self.expr_sort(lhs);
        let rhs_sort = self.expr_sort(rhs);
        if lhs_sort != rhs_sort {
            panic!(
                "{} expected matching sorts, got {:?} and {:?}",
                op, lhs_sort, rhs_sort
            );
        }
    }

    fn expect_int_operands(&self, lhs: &SymExpr, rhs: &SymExpr, op: &str) {
        let lhs_sort = self.expr_sort(lhs);
        let rhs_sort = self.expr_sort(rhs);
        if lhs_sort != SymSort::Int || rhs_sort != SymSort::Int {
            panic!(
                "{} expected Int operands, got {:?} and {:?}",
                op, lhs_sort, rhs_sort
            );
        }
    }
}
