use crate::symbolic::{BoundVar, BoundVarId, SymExpr, SymFunc, SymSort};
use std::collections::HashMap;
use z3::ast::{Ast, Bool, Dynamic, Int};
use z3::{FuncDecl, Sort, Symbol};

type BoundEnv = HashMap<BoundVarId, Dynamic>;

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
        match self.solver.check() {
            z3::SatResult::Sat => true,
            z3::SatResult::Unsat => false,
            z3::SatResult::Unknown => panic!("Z3 returned unknown for symbolic constraint"),
        }
    }

    fn compile_bool(&self, expr: &SymExpr) -> Bool {
        self.compile_bool_with_env(expr, &mut HashMap::new())
    }

    fn compile_bool_with_env(&self, expr: &SymExpr, env: &mut BoundEnv) -> Bool {
        self.compile_dynamic(expr, env)
            .as_bool()
            .unwrap_or_else(|| panic!("expected symbolic bool expression, got {:?}", expr))
    }

    fn compile_int_with_env(&self, expr: &SymExpr, env: &mut BoundEnv) -> Int {
        self.compile_dynamic(expr, env)
            .as_int()
            .unwrap_or_else(|| panic!("expected symbolic int expression, got {:?}", expr))
    }

    fn compile_dynamic(&self, expr: &SymExpr, env: &mut BoundEnv) -> Dynamic {
        match expr {
            SymExpr::Var {
                id,
                sort: SymSort::Bool,
            } => Dynamic::from_ast(&Bool::new_const(format!("sym_b_{}", id.suffix()))),
            SymExpr::Var {
                id,
                sort: SymSort::Int,
            } => Dynamic::from_ast(&Int::new_const(format!("sym_i_{}", id.suffix()))),
            SymExpr::Var {
                id,
                sort: sort @ SymSort::Uninterpreted(_),
            } => {
                let sort = self.compile_sort(sort);
                Dynamic::new_const(format!("sym_u_{}", id.suffix()), &sort)
            }
            SymExpr::BoundVar { id, .. } => self.lookup_bound(env, id),
            SymExpr::Bool(value) => Dynamic::from_ast(&Bool::from_bool(*value)),
            SymExpr::Int(value) => Dynamic::from_ast(&Int::from_i64(*value)),
            SymExpr::App { func, args } => self.compile_app(func, args, env),
            SymExpr::Forall { vars, body } => self.compile_quantifier(true, vars, body, env),
            SymExpr::Exists { vars, body } => self.compile_quantifier(false, vars, body, env),
            SymExpr::Not(inner) => Dynamic::from_ast(&self.compile_bool_with_env(inner, env).not()),
            SymExpr::And(lhs, rhs) => {
                let lhs = self.compile_bool_with_env(lhs, env);
                let rhs = self.compile_bool_with_env(rhs, env);
                Dynamic::from_ast(&Bool::and(&[lhs, rhs]))
            }
            SymExpr::Or(lhs, rhs) => {
                let lhs = self.compile_bool_with_env(lhs, env);
                let rhs = self.compile_bool_with_env(rhs, env);
                Dynamic::from_ast(&Bool::or(&[lhs, rhs]))
            }
            SymExpr::Implies(lhs, rhs) => {
                let lhs = self.compile_bool_with_env(lhs, env);
                let rhs = self.compile_bool_with_env(rhs, env);
                Dynamic::from_ast(&lhs.implies(rhs))
            }
            SymExpr::Eq(lhs, rhs) => {
                self.expect_same_sort(lhs, rhs, "equality");
                let lhs = self.compile_dynamic(lhs, env);
                let rhs = self.compile_dynamic(rhs, env);
                Dynamic::from_ast(&lhs.eq(&rhs))
            }
            SymExpr::Gt(lhs, rhs) => {
                let lhs = self.compile_int_with_env(lhs, env);
                let rhs = self.compile_int_with_env(rhs, env);
                Dynamic::from_ast(&lhs.gt(rhs))
            }
            SymExpr::Ge(lhs, rhs) => {
                let lhs = self.compile_int_with_env(lhs, env);
                let rhs = self.compile_int_with_env(rhs, env);
                Dynamic::from_ast(&lhs.ge(rhs))
            }
            SymExpr::Lt(lhs, rhs) => {
                let lhs = self.compile_int_with_env(lhs, env);
                let rhs = self.compile_int_with_env(rhs, env);
                Dynamic::from_ast(&lhs.lt(rhs))
            }
            SymExpr::Le(lhs, rhs) => {
                let lhs = self.compile_int_with_env(lhs, env);
                let rhs = self.compile_int_with_env(rhs, env);
                Dynamic::from_ast(&lhs.le(rhs))
            }
            SymExpr::Add(lhs, rhs) => {
                self.expect_int_operands(lhs, rhs, "addition");
                let lhs = self.compile_int_with_env(lhs, env);
                let rhs = self.compile_int_with_env(rhs, env);
                Dynamic::from_ast(&Int::add(&[lhs, rhs]))
            }
            SymExpr::Sub(lhs, rhs) => {
                self.expect_int_operands(lhs, rhs, "subtraction");
                let lhs = self.compile_int_with_env(lhs, env);
                let rhs = self.compile_int_with_env(rhs, env);
                Dynamic::from_ast(&Int::sub(&[lhs, rhs]))
            }
            SymExpr::Mul(lhs, rhs) => {
                self.expect_int_operands(lhs, rhs, "multiplication");
                let lhs = self.compile_int_with_env(lhs, env);
                let rhs = self.compile_int_with_env(rhs, env);
                Dynamic::from_ast(&Int::mul(&[lhs, rhs]))
            }
            SymExpr::Div(lhs, rhs) => {
                self.expect_int_operands(lhs, rhs, "division");
                let lhs = self.compile_int_with_env(lhs, env);
                let rhs = self.compile_int_with_env(rhs, env);
                Dynamic::from_ast(&lhs.div(rhs))
            }
            SymExpr::Rem(lhs, rhs) => {
                self.expect_int_operands(lhs, rhs, "remainder");
                let lhs = self.compile_int_with_env(lhs, env);
                let rhs = self.compile_int_with_env(rhs, env);
                Dynamic::from_ast(&lhs.rem(rhs))
            }
        }
    }

    fn expr_sort(&self, expr: &SymExpr) -> SymSort {
        match expr {
            SymExpr::Var { sort, .. } | SymExpr::BoundVar { sort, .. } => sort.clone(),
            SymExpr::Bool(_)
            | SymExpr::Not(_)
            | SymExpr::And(_, _)
            | SymExpr::Or(_, _)
            | SymExpr::Implies(_, _)
            | SymExpr::Forall { .. }
            | SymExpr::Exists { .. }
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

    fn compile_app(&self, func: &SymFunc, args: &[SymExpr], env: &mut BoundEnv) -> Dynamic {
        self.validate_app(func, args);

        let decl = self.compile_func(func);
        let args = args
            .iter()
            .map(|arg| self.compile_dynamic(arg, env))
            .collect::<Vec<_>>();
        let arg_refs = args.iter().map(|arg| arg as &dyn Ast).collect::<Vec<_>>();

        decl.apply(&arg_refs)
    }

    fn compile_quantifier(
        &self,
        is_forall: bool,
        vars: &[BoundVar],
        body: &SymExpr,
        env: &mut BoundEnv,
    ) -> Dynamic {
        let body_sort = self.expr_sort(body);
        if body_sort != SymSort::Bool {
            panic!("quantifier body must be Bool, got {:?}", body_sort);
        }

        let z3_vars = vars
            .iter()
            .map(|var| {
                let sort = self.compile_sort(var.sort());
                Dynamic::new_const(
                    format!("q_{}_{}_{}", var.name(), var.id().scope(), var.id().index()),
                    &sort,
                )
            })
            .collect::<Vec<_>>();

        for (var, z3_var) in vars.iter().zip(z3_vars.iter()) {
            env.insert(var.id(), z3_var.clone());
        }

        let body = self.compile_bool_with_env(body, env);

        for var in vars {
            env.remove(&var.id());
        }

        let var_refs = z3_vars
            .iter()
            .map(|var| var as &dyn Ast)
            .collect::<Vec<_>>();

        if is_forall {
            Dynamic::from_ast(&z3::ast::forall_const(&var_refs, &[], &body))
        } else {
            Dynamic::from_ast(&z3::ast::exists_const(&var_refs, &[], &body))
        }
    }

    fn lookup_bound(&self, env: &BoundEnv, id: &BoundVarId) -> Dynamic {
        env.get(id)
            .cloned()
            .unwrap_or_else(|| panic!("unbound quantified variable {:?}", id))
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
