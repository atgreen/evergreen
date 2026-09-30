// Mirror source for crate-local spec path checks.
pub struct Frame {
    pub prev_fp: *mut Frame,
}

pub enum FrameType {
    Special,
}

impl FrameType {
    pub fn marker() -> Self {
        FrameType::Special
    }
}

pub struct EgclStack;

impl EgclStack {
    pub fn publish_top(&self) {}
}

pub struct CodeInfo;

impl CodeInfo {
    pub fn stack_map(&self, pc_offset: usize) -> Option<&[u8]> {
        let _ = pc_offset;
        None
    }
}
