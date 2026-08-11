// Mirror source for crate-local spec path checks.
pub struct BlissError;
pub struct SelfType;

pub fn from_env() -> Result<SelfType, BlissError> {
    Err(BlissError)
}

pub fn apply_cli_args(&mut self, args: &[String]) {}

pub fn parse_cli() {}

pub fn shutdown(&mut self) -> Result<(), BlissError> {
    Err(BlissError)
}

pub fn install_signal_handlers() {}

fn _markers() {
    let _ = "pub fn from_env() -> Result<Self, BlissError>";
    let _ = "pub fn apply_cli_args(&mut self, args: &[String])";
    let _ = "pub fn parse_cli";
    let _ = "pub fn shutdown(&mut self) -> Result<(), BlissError>";
    let _ = "install_signal_handlers";
    let _ = "SIGINT_RECEIVED.store";
    let _ = "SIGSEGV";
}
