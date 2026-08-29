//! A fixture, not real code.

use std::fmt;

pub const LIMIT: usize = 8;

pub struct Widget {
    id: u32,
}

pub enum Shape {
    Round,
    Square,
}

pub trait Named {
    fn name(&self) -> &str;
}

impl Named for Widget {
    fn name(&self) -> &str {
        "widget"
    }
}

impl Widget {
    pub fn new(id: u32) -> Self {
        Self { id }
    }

    pub fn id(&self) -> u32 {
        self.id
    }
}

pub mod inner {
    pub fn helper() -> bool {
        true
    }
}

pub fn build() -> Widget {
    Widget::new(1)
}
