//! Type erasure: the engine books any mix of registered object types in one
//! call, so it works on objects behind one trait, and a registry maps a type
//! name (from a snapshot diff) back to the reader and accounting that decode
//! and evaluate it.

use std::any::Any;
use std::collections::HashMap;
use std::marker::PhantomData;

use serde_json::Value;

use crate::accounting::Accounting;
use crate::balances::Balances;
use crate::desired::Desired;
use crate::error::Rejection;
use crate::object::LedgerObject;
use crate::types::{ObjectId, ProcessId};

/// An object of some registered type, with its accounting.
pub(crate) trait ErasedObject: Send + Sync {
    fn type_name(&self) -> &'static str;
    fn process_id(&self) -> ProcessId;
    fn object_id(&self) -> ObjectId;
    fn version(&self) -> u64;
    fn rules_version(&self) -> u32;
    fn to_json(&self) -> Result<Value, Rejection>;
    /// Same type and same state.
    fn state_eq(&self, other: &dyn ErasedObject) -> bool;
    /// `self`'s state is among the allowed next states from `current`'s
    /// (or from "not booked" when `current` is `None`).
    fn change_allowed(&self, current: Option<&dyn ErasedObject>) -> bool;
    /// `self`'s state lists itself: amounts may change while in it.
    fn amount_change_allowed(&self) -> bool;
    /// `desired_state` of `self`'s own state on `self`.
    fn desired(&self) -> Desired;
    /// `validate_balances` of `self`'s own state.
    fn validate_balances(&self, balances: &Balances, desired: &Desired) -> Result<(), Rejection>;
    fn describe_state(&self) -> String;
    fn as_any(&self) -> &dyn Any;
}

pub(crate) struct Typed<O, A> {
    object: O,
    _accounting: PhantomData<fn() -> A>,
}

impl<O: LedgerObject, A: Accounting<O>> Typed<O, A> {
    pub(crate) fn new(object: O) -> Self {
        Typed {
            object,
            _accounting: PhantomData,
        }
    }

    fn state(&self) -> O::State {
        self.object.state()
    }
}

impl<O: LedgerObject, A: Accounting<O>> ErasedObject for Typed<O, A> {
    fn type_name(&self) -> &'static str {
        O::TYPE
    }

    fn process_id(&self) -> ProcessId {
        self.object.process_id()
    }

    fn object_id(&self) -> ObjectId {
        self.object.object_id()
    }

    fn version(&self) -> u64 {
        self.object.version()
    }

    fn rules_version(&self) -> u32 {
        A::VERSION
    }

    fn to_json(&self) -> Result<Value, Rejection> {
        serde_json::to_value(&self.object)
            .map_err(|e| Rejection::new(format!("object {} does not serialize: {e}", O::TYPE)))
    }

    fn state_eq(&self, other: &dyn ErasedObject) -> bool {
        other
            .as_any()
            .downcast_ref::<Typed<O, A>>()
            .is_some_and(|o| o.state() == self.state())
    }

    fn change_allowed(&self, current: Option<&dyn ErasedObject>) -> bool {
        let current = match current {
            None => None,
            Some(c) => match c.as_any().downcast_ref::<Typed<O, A>>() {
                Some(c) => Some(c.state()),
                None => return false,
            },
        };
        A::validate_state_change(current).contains(&self.state())
    }

    fn amount_change_allowed(&self) -> bool {
        let state = self.state();
        A::validate_state_change(Some(state)).contains(&state)
    }

    fn desired(&self) -> Desired {
        A::desired_state(self.state(), &self.object)
    }

    fn validate_balances(&self, balances: &Balances, desired: &Desired) -> Result<(), Rejection> {
        A::validate_balances(self.state(), &self.object, balances, desired)
    }

    fn describe_state(&self) -> String {
        format!("{:?}", self.state())
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// One registered object type.
pub(crate) trait TypeHandler: Send + Sync {
    fn type_name(&self) -> &'static str;
    fn rules_version(&self) -> u32;
    /// From a folded snapshot.
    fn decode(&self, json: &Value) -> Result<Box<dyn ErasedObject>, String>;
    /// From a submitted object, or `None` when the `Any` is not this type.
    fn wrap(&self, object: Box<dyn Any + Send>) -> Option<Box<dyn ErasedObject>>;
}

struct Handler<O, A>(PhantomData<fn() -> (O, A)>);

impl<O: LedgerObject, A: Accounting<O>> TypeHandler for Handler<O, A> {
    fn type_name(&self) -> &'static str {
        O::TYPE
    }

    fn rules_version(&self) -> u32 {
        A::VERSION
    }

    fn decode(&self, json: &Value) -> Result<Box<dyn ErasedObject>, String> {
        let object: O = serde_json::from_value(json.clone()).map_err(|e| e.to_string())?;
        Ok(Box::new(Typed::<O, A>::new(object)))
    }

    fn wrap(&self, object: Box<dyn Any + Send>) -> Option<Box<dyn ErasedObject>> {
        let object = object.downcast::<O>().ok()?;
        Some(Box::new(Typed::<O, A>::new(*object)))
    }
}

/// The registered types, by name.
#[derive(Default)]
pub(crate) struct Registry {
    handlers: HashMap<&'static str, Box<dyn TypeHandler>>,
}

impl Registry {
    pub(crate) fn register<O: LedgerObject, A: Accounting<O>>(&mut self) -> Result<(), String> {
        if self.handlers.contains_key(O::TYPE) {
            return Err(format!("object type {:?} registered twice", O::TYPE));
        }
        self.handlers
            .insert(O::TYPE, Box::new(Handler::<O, A>(PhantomData)));
        Ok(())
    }

    pub(crate) fn get(&self, type_name: &str) -> Option<&dyn TypeHandler> {
        self.handlers.get(type_name).map(|h| h.as_ref())
    }

    pub(crate) fn types(&self) -> impl Iterator<Item = (&'static str, u32)> + '_ {
        self.handlers
            .values()
            .map(|h| (h.type_name(), h.rules_version()))
    }
}
