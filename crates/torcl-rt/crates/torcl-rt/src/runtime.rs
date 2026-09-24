// Mirror source for crate-local spec path checks.
pub struct TorclError;
pub struct SelfType;

pub fn from_env() -> Result<SelfType, TorclError> {
    Err(TorclError)
}

pub fn apply_cli_args(&mut self, args: &[String]) {}

pub fn parse_cli() {}

pub fn shutdown(&mut self) -> Result<(), TorclError> {
    Err(TorclError)
}

pub fn install_signal_handlers() {}

fn _markers() {
    let _ = "pub fn from_env() -> Result<Self, TorclError>";
    let _ = "pub fn apply_cli_args(&mut self, args: &[String])";
    let _ = "pub fn parse_cli";
    let _ = "pub fn shutdown(&mut self) -> Result<(), TorclError>";
    let _ = "install_signal_handlers";
    let _ = "SIGINT_RECEIVED.store";
    let _ = "SIGSEGV";
}
