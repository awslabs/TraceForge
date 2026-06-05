//! Must's requirements for types passed as messages

use dyn_clone::DynClone;
use std::any::Any;

/// This type is used to signify the type of messages
/// exchanged between threads as well as the type of
/// values returned by a thread.
/// Since we do not know this type in advance, we created
/// a trait `Message` that signifies (Clone, Debug, PartialEq)
/// that needs to be derived on such values of type `Val`.
///
/// This is `pub` because monitors, which are instantiated
/// via macros in the customer's crate, receive values of
/// this type.
#[derive(Clone, Debug)]
pub struct Val {
    pub(crate) val: Box<dyn Message>,
    pub type_name: String,
}

impl PartialEq for Val {
    fn eq(&self, other: &Self) -> bool {
        self.val.msg_equals(other)
    }
}

impl Val {
    pub fn new<T: Message + 'static>(val: T) -> Self {
        Val {
            val: Box::new(val),
            type_name: std::any::type_name::<T>().to_string(),
        }
    }

    pub(crate) fn set_pending(&mut self) {
        *self = Self::default();
    }

    pub(crate) fn is_pending(&self) -> bool {
        self.val.msg_as_any_ref().is::<Unit>()
    }

    pub fn as_any(&self) -> Box<dyn Any> {
        self.val.clone().msg_as_any()
    }

    /// A compact rendering of just the contained value, without the surrounding
    /// `Val { .. }` wrapper or the `type_name`. Used for concise graph labels.
    /// Returns an empty string when value printing is disabled at compile time.
    pub(crate) fn display_value(&self) -> String {
        #[cfg(feature = "print_vals_custom")]
        {
            format!("{}", self.val)
        }
        #[cfg(all(feature = "print_vals", not(feature = "print_vals_custom")))]
        {
            format!("{:?}", self.val)
        }
        #[cfg(not(any(feature = "print_vals", feature = "print_vals_custom")))]
        {
            String::new()
        }
    }

    pub fn as_any_ref(&self) -> &dyn Any {
        self.val.msg_as_any_ref()
    }
}

impl Default for Val {
    fn default() -> Self {
        Val::new(Unit)
    }
}

/// This is a default type introduced for values of type `Val`.
/// This comes in handy when serializing an execution graph
/// when values of type `Val` cannot be serialized.
/// In such situations, those values are replaced by `Unit`.
/// This *looks like* the Rust built-in unit type () but it is a
/// special Must type that can serve as a sentry value for show when
/// a thread's result is missing from the graph because it was ignored
/// during serialization.
#[derive(Clone, PartialEq, Debug)]
struct Unit;

impl std::fmt::Display for Unit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "()")
    }
}

macro_rules! sign_msg_core {
    () => {
        // Prefix these types with 'msg' so that their use must be
        // chosen explicitly rather than by accident or confusion that
        // someone is calling Box::as_any() or similar things.
        fn msg_as_any(self: Box<Self>) -> Box<dyn Any>;
        fn msg_as_any_ref(&self) -> &dyn Any;
        fn msg_equals(&self, other: &crate::Val) -> bool;
    };
}

/// Supertrait to be satisfied by all messages exchanged in user
/// programs. Assuming a user type `T` satisfies `Clone` and
/// `PartialEq`, `Message` is automatically derived.
// TODO Any way we can get around PartialEq and still get symmetry
// reduction?
#[cfg(not(any(feature = "print_vals", feature = "print_vals_custom")))]
pub trait Message: Send + DynClone {
    sign_msg_core!();
}

#[cfg(feature = "print_vals")]
pub trait Message: Send + DynClone + std::fmt::Debug {
    sign_msg_core!();
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result;
}

#[cfg(feature = "print_vals_custom")]
pub trait Message: Send + DynClone + std::fmt::Display {
    sign_msg_core!();
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result;
}

macro_rules! impl_msg_core {
    () => {
        fn msg_as_any(self: Box<Self>) -> Box<dyn Any> {
            self
        }

        fn msg_as_any_ref(&self) -> &dyn Any {
            self
        }

        fn msg_equals(&self, other: &crate::Val) -> bool {
            match other.as_any_ref().downcast_ref::<T>() {
                Some(a) => self == a,
                None => false,
            }
        }
    };
}

#[cfg(not(any(feature = "print_vals", feature = "print_vals_custom")))]
impl<T: Send + PartialEq + DynClone + std::fmt::Debug + 'static> Message for T {
    impl_msg_core!();
}

#[cfg(feature = "print_vals")]
impl<T: Send + PartialEq + DynClone + std::fmt::Debug + 'static> Message for T {
    impl_msg_core!();

    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(
            f,
            "{:?}",
            self.msg_as_any_ref().downcast_ref::<T>().unwrap()
        )
    }
}

#[cfg(feature = "print_vals_custom")]
impl<T: Send + PartialEq + DynClone + std::fmt::Display + 'static> Message for T {
    impl_msg_core!();

    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(f, "{}", self.msg_as_any_ref().downcast_ref::<T>().unwrap())
    }
}

#[cfg(all(feature = "print_vals", feature = "print_vals_custom"))]
compile_error!("features `must/print_vals` and `must/print_vals_custom` are mutually exclusive");

dyn_clone::clone_trait_object!(Message);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_unit_display() {
        let unit = Unit;
        assert_eq!(format!("{}", unit), "()");
    }

    #[test]
    fn test_val_partial_eq() {
        let val1 = Val::new(42);
        let val2 = Val::new(42);
        let val3 = Val::new(43);

        assert!(val1 == val2);
        assert!(!(val1 == val3));
    }

    #[test]
    fn test_val_partial_eq_different_types() {
        let val1 = Val::new(42);
        let val2 = Val::new("hello");

        assert!(!(val1 == val2));
    }

    #[test]
    fn test_val_partial_eq_null() {
        let val1: Val = Val::default();
        let val2: Val = Val::default();

        assert!(val1 == val2);
    }
}
