// Mirror source for crate-local spec path checks.
pub struct FiberId;

pub enum BlissError {
    StackOverflow(FiberId),
}
