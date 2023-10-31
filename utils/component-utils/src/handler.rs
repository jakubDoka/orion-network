use std::sync::{Arc, Weak};

#[derive(Default, Debug)]
pub struct HandlerCore(Arc<()>);

impl HandlerCore {
    pub fn has_no_trafic(&self) -> bool {
        Arc::weak_count(&self.0) == 0
    }

    pub fn take_ref(&self) -> HandlerRef {
        HandlerRef(Arc::downgrade(&self.0))
    }
}

#[derive(Clone, Debug)]
pub struct HandlerRef(Weak<()>);

impl HandlerRef {
    pub fn is_invalid(&self) -> bool {
        self.0.strong_count() == 0
    }
}
