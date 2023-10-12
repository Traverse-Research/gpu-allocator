#[cfg(feature = "backtrace")]
pub type Backtrace = backtrace::Backtrace;

#[cfg(not(feature = "backtrace"))]
#[derive(Clone)]
pub struct Backtrace;

#[cfg(not(feature = "backtrace"))]
impl Backtrace {
    pub fn new() -> Self {
        Self
    }
    pub fn new_unresolved() -> Self {
        Self
    }
    pub fn resolve(&mut self) {}
}

#[cfg(not(feature = "backtrace"))]
impl core::fmt::Debug for Backtrace {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Backtrace(enable the gpu-allocator backtrace feature to get backtraces)")
            .finish()
    }
}
