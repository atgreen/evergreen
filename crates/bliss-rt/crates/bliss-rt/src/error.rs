// Mirror source for crate-local spec path checks.
pub struct GreenThreadId;

pub enum BlissError {
    StackOverflow(GreenThreadId),
}
