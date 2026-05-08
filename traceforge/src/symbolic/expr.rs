use crate::event::Event;
use serde::{Deserialize, Serialize};
use std::cell::Cell;
use std::ops::{Add, Div, Mul, Rem, Sub};
use std::rc::Rc;

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SymVarId(Event);

impl SymVarId {
    pub(crate) fn from_event(pos: Event) -> Self {
        Self(pos)
    }

    pub(crate) fn suffix(&self) -> String {
        format!("{}_{}", self.0.thread, self.0.index)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum SymSort {
    Bool,
    Int,
    Uninterpreted(String),
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SymFunc {
    name: String,
    domain: Vec<SymSort>,
    range: SymSort,
}

impl SymFunc {
    pub fn new(name: impl Into<String>, domain: Vec<SymSort>, range: SymSort) -> Self {
        Self {
            name: name.into(),
            domain,
            range,
        }
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn domain(&self) -> &[SymSort] {
        &self.domain
    }

    pub fn range(&self) -> &SymSort {
        &self.range
    }

    pub fn apply(&self, args: impl IntoIterator<Item = SymExpr>) -> SymExpr {
        SymExpr::App {
            func: self.clone(),
            args: args.into_iter().collect(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct BoundVarId {
    scope: usize,
    index: usize,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct BoundVar {
    id: BoundVarId,
    name: String,
    sort: SymSort,
}

impl BoundVar {
    pub fn expr(&self) -> SymExpr {
        SymExpr::BoundVar {
            id: self.id,
            name: self.name.clone(),
            sort: self.sort.clone(),
        }
    }

    pub(crate) fn id(&self) -> BoundVarId {
        self.id
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn sort(&self) -> &SymSort {
        &self.sort
    }
}

impl BoundVarId {
    pub(crate) fn scope(&self) -> usize {
        self.scope
    }

    pub(crate) fn index(&self) -> usize {
        self.index
    }
}

pub struct BoundVars {
    scopes: Vec<Vec<BoundVar>>,
    next_scope: Rc<Cell<usize>>,
}

impl BoundVars {
    pub fn get(&self, name: &str) -> SymExpr {
        self.scopes
            .iter()
            .find_map(|scope| {
                scope
                    .iter()
                    .find(|var| var.name() == name)
                    .map(|var| var.expr())
            })
            .unwrap_or_else(|| panic!("unknown bound variable `{name}`"))
    }

    pub fn forall<const N: usize>(
        &self,
        vars: [(&str, SymSort); N],
        body: impl FnOnce(&BoundVars) -> SymExpr,
    ) -> SymExpr {
        quantified_with_scopes(true, &self.scopes, self.next_scope.clone(), vars, body)
    }

    pub fn exists<const N: usize>(
        &self,
        vars: [(&str, SymSort); N],
        body: impl FnOnce(&BoundVars) -> SymExpr,
    ) -> SymExpr {
        quantified_with_scopes(false, &self.scopes, self.next_scope.clone(), vars, body)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum SymExpr {
    Var {
        id: SymVarId,
        sort: SymSort,
    },
    BoundVar {
        id: BoundVarId,
        name: String,
        sort: SymSort,
    },
    Bool(bool),
    Int(i64),
    App {
        func: SymFunc,
        args: Vec<SymExpr>,
    },
    Forall {
        vars: Vec<BoundVar>,
        body: Box<SymExpr>,
    },
    Exists {
        vars: Vec<BoundVar>,
        body: Box<SymExpr>,
    },
    Not(Box<SymExpr>),
    And(Box<SymExpr>, Box<SymExpr>),
    Or(Box<SymExpr>, Box<SymExpr>),
    Implies(Box<SymExpr>, Box<SymExpr>),
    Eq(Box<SymExpr>, Box<SymExpr>),
    Gt(Box<SymExpr>, Box<SymExpr>),
    Ge(Box<SymExpr>, Box<SymExpr>),
    Lt(Box<SymExpr>, Box<SymExpr>),
    Le(Box<SymExpr>, Box<SymExpr>),
    Add(Box<SymExpr>, Box<SymExpr>),
    Sub(Box<SymExpr>, Box<SymExpr>),
    Mul(Box<SymExpr>, Box<SymExpr>),
    Div(Box<SymExpr>, Box<SymExpr>),
    Rem(Box<SymExpr>, Box<SymExpr>),
}

impl SymExpr {
    pub fn not(self) -> Self {
        SymExpr::Not(Box::new(self))
    }

    pub fn and(self, other: impl Into<SymExpr>) -> Self {
        SymExpr::And(Box::new(self), Box::new(other.into()))
    }

    pub fn or(self, other: impl Into<SymExpr>) -> Self {
        SymExpr::Or(Box::new(self), Box::new(other.into()))
    }

    pub fn implies(self, other: impl Into<SymExpr>) -> Self {
        SymExpr::Implies(Box::new(self), Box::new(other.into()))
    }

    pub fn equals(self, other: impl Into<SymExpr>) -> Self {
        SymExpr::Eq(Box::new(self), Box::new(other.into()))
    }

    pub fn gt(self, other: impl Into<SymExpr>) -> Self {
        SymExpr::Gt(Box::new(self), Box::new(other.into()))
    }

    pub fn ge(self, other: impl Into<SymExpr>) -> Self {
        SymExpr::Ge(Box::new(self), Box::new(other.into()))
    }

    pub fn lt(self, other: impl Into<SymExpr>) -> Self {
        SymExpr::Lt(Box::new(self), Box::new(other.into()))
    }

    pub fn le(self, other: impl Into<SymExpr>) -> Self {
        SymExpr::Le(Box::new(self), Box::new(other.into()))
    }

    pub fn sub(self, other: impl Into<SymExpr>) -> Self {
        SymExpr::Sub(Box::new(self), Box::new(other.into()))
    }

    pub fn mul(self, other: impl Into<SymExpr>) -> Self {
        SymExpr::Mul(Box::new(self), Box::new(other.into()))
    }

    pub fn div(self, other: impl Into<SymExpr>) -> Self {
        SymExpr::Div(Box::new(self), Box::new(other.into()))
    }

    pub fn rem(self, other: impl Into<SymExpr>) -> Self {
        SymExpr::Rem(Box::new(self), Box::new(other.into()))
    }
}

pub fn int_val(v: i64) -> SymExpr {
    SymExpr::Int(v)
}

pub fn bool_val(v: bool) -> SymExpr {
    SymExpr::Bool(v)
}

pub fn uninterpreted_sort(name: impl Into<String>) -> SymSort {
    SymSort::Uninterpreted(name.into())
}

pub fn uf(name: impl Into<String>, domain: &[SymSort], range: SymSort) -> SymFunc {
    SymFunc::new(name, domain.to_vec(), range)
}

pub fn predicate(name: impl Into<String>, domain: &[SymSort]) -> SymFunc {
    uf(name, domain, SymSort::Bool)
}

pub fn constant(name: impl Into<String>, sort: SymSort) -> SymExpr {
    SymFunc::new(name, Vec::new(), sort).apply([])
}

pub fn forall<const N: usize>(
    vars: [(&str, SymSort); N],
    body: impl FnOnce(&BoundVars) -> SymExpr,
) -> SymExpr {
    quantified(true, vars, body)
}

pub fn exists<const N: usize>(
    vars: [(&str, SymSort); N],
    body: impl FnOnce(&BoundVars) -> SymExpr,
) -> SymExpr {
    quantified(false, vars, body)
}

fn quantified<const N: usize>(
    is_forall: bool,
    vars: [(&str, SymSort); N],
    body: impl FnOnce(&BoundVars) -> SymExpr,
) -> SymExpr {
    quantified_with_scopes(is_forall, &[], Rc::new(Cell::new(0)), vars, body)
}

fn quantified_with_scopes<const N: usize>(
    is_forall: bool,
    outer_scopes: &[Vec<BoundVar>],
    next_scope: Rc<Cell<usize>>,
    vars: [(&str, SymSort); N],
    body: impl FnOnce(&BoundVars) -> SymExpr,
) -> SymExpr {
    let scope = next_scope.get();
    next_scope.set(scope + 1);

    let vars = vars
        .into_iter()
        .enumerate()
        .map(|(index, (name, sort))| BoundVar {
            id: BoundVarId { scope, index },
            name: name.to_string(),
            sort,
        })
        .collect::<Vec<_>>();

    let mut scopes = Vec::with_capacity(outer_scopes.len() + 1);
    scopes.push(vars.clone());
    scopes.extend_from_slice(outer_scopes);

    let bound_vars = BoundVars { scopes, next_scope };
    let body = body(&bound_vars);

    if is_forall {
        SymExpr::Forall {
            vars,
            body: Box::new(body),
        }
    } else {
        SymExpr::Exists {
            vars,
            body: Box::new(body),
        }
    }
}

impl From<i64> for SymExpr {
    fn from(value: i64) -> Self {
        int_val(value)
    }
}

impl From<bool> for SymExpr {
    fn from(value: bool) -> Self {
        bool_val(value)
    }
}

impl Add for SymExpr {
    type Output = SymExpr;

    fn add(self, rhs: SymExpr) -> Self::Output {
        SymExpr::Add(Box::new(self), Box::new(rhs))
    }
}

impl Add<i64> for SymExpr {
    type Output = SymExpr;

    fn add(self, rhs: i64) -> Self::Output {
        self + int_val(rhs)
    }
}

impl Add<SymExpr> for i64 {
    type Output = SymExpr;

    fn add(self, rhs: SymExpr) -> Self::Output {
        int_val(self) + rhs
    }
}

impl Sub for SymExpr {
    type Output = SymExpr;

    fn sub(self, rhs: SymExpr) -> Self::Output {
        SymExpr::Sub(Box::new(self), Box::new(rhs))
    }
}

impl Sub<i64> for SymExpr {
    type Output = SymExpr;

    fn sub(self, rhs: i64) -> Self::Output {
        self - int_val(rhs)
    }
}

impl Sub<SymExpr> for i64 {
    type Output = SymExpr;

    fn sub(self, rhs: SymExpr) -> Self::Output {
        int_val(self) - rhs
    }
}

impl Mul for SymExpr {
    type Output = SymExpr;

    fn mul(self, rhs: SymExpr) -> Self::Output {
        SymExpr::Mul(Box::new(self), Box::new(rhs))
    }
}

impl Mul<i64> for SymExpr {
    type Output = SymExpr;

    fn mul(self, rhs: i64) -> Self::Output {
        self * int_val(rhs)
    }
}

impl Mul<SymExpr> for i64 {
    type Output = SymExpr;

    fn mul(self, rhs: SymExpr) -> Self::Output {
        int_val(self) * rhs
    }
}

impl Div for SymExpr {
    type Output = SymExpr;

    fn div(self, rhs: SymExpr) -> Self::Output {
        SymExpr::Div(Box::new(self), Box::new(rhs))
    }
}

impl Div<i64> for SymExpr {
    type Output = SymExpr;

    fn div(self, rhs: i64) -> Self::Output {
        self / int_val(rhs)
    }
}

impl Div<SymExpr> for i64 {
    type Output = SymExpr;

    fn div(self, rhs: SymExpr) -> Self::Output {
        int_val(self) / rhs
    }
}

impl Rem for SymExpr {
    type Output = SymExpr;

    fn rem(self, rhs: SymExpr) -> Self::Output {
        SymExpr::Rem(Box::new(self), Box::new(rhs))
    }
}

impl Rem<i64> for SymExpr {
    type Output = SymExpr;

    fn rem(self, rhs: i64) -> Self::Output {
        self % int_val(rhs)
    }
}

impl Rem<SymExpr> for i64 {
    type Output = SymExpr;

    fn rem(self, rhs: SymExpr) -> Self::Output {
        int_val(self) % rhs
    }
}
