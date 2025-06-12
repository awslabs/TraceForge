use core::fmt::Debug;
use std::hash::Hash;

use dyn_clone::DynClone;
use dyn_eq::DynEq;
use dyn_hash::DynHash;

pub trait Identifier: DynEq + DynClone + DynHash + Debug + Send {}
dyn_clone::clone_trait_object!(Identifier);
dyn_hash::hash_trait_object!(Identifier);
dyn_eq::eq_trait_object!(Identifier);

impl<T: Eq + Clone + Hash + Debug + Send + 'static> Identifier for T {}

// TODO:
// - Add an unsafe Send + Sync implementation, (there's no actual concurrency)
// - Reconsider various clones (see channel) that are now not necessary
// - Make Receiver take a mutable reference to avoid accidental
//   concurrenct (porf-unordered) receives
