use std::fmt::Debug as DebugTrait;

mod nested {
    pub fn nested_helper() {}
}

struct Widget;
enum State {
    Ready,
    Done,
}

trait Runner {
    fn run(&self);

    fn defaulted(&self) {
        helper_call();
    }
}

impl Runner for Widget {
    fn run(&self) {
        helper_call();
        self.method();
    }
}

impl Widget {
    fn method(&self) {}
}

type WidgetAlias = Widget;
const LIMIT: usize = 10;
static ENABLED: bool = true;

macro_rules! announce {
    () => {};
}

fn helper_call() {}

fn main() {
    helper_call();
}

#[test]
fn parses_repository() {
    helper_call();
}

#[bench]
fn measures_repository() {}
