use serde::de::Visitor;
use serde::{Deserialize, Serialize};

use crate::thread::ThreadId;

use std::fmt::Debug;
use std::sync::Arc;

/// This type represents the type of predicates on message tags.
/// A separate type helps in serialization as it isolates all
/// the code relevant to serializing/deserializing tag predicates.
#[derive(Clone)]
pub(crate) struct PredicateType(pub Arc<dyn Send + Sync + Fn(ThreadId, Option<Vec<u32>>) -> bool>);

impl Serialize for PredicateType {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_unit()
    }
}

impl Debug for PredicateType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "<tag_predicate>")
    }
}

struct UnitVisitor;

impl<'de> Visitor<'de> for UnitVisitor {
    type Value = ();

    fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
        formatter.write_str("a unit value")
    }

    fn visit_unit<E>(self) -> Result<Self::Value, E>
    where
        E: serde::de::Error,
    {
        Ok(())
    }
}

impl<'de> Deserialize<'de> for PredicateType {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let des = deserializer.deserialize_unit(UnitVisitor);
        match des {
            Ok(_u) => Ok(PredicateType(Arc::new(|_, _| true))),
            Err(e) => Err(e),
        }
    }
}

pub(crate) fn normalize_vec_tag(tag: Option<Vec<u32>>) -> Option<Vec<u32>> {
    match tag {
        Some(tag) if tag.is_empty() => None,
        other => other,
    }
}
