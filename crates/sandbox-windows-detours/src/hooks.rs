    #![cfg(windows)]

    use std::io;

    /// Install Detours-based file-system hooks in the current process.
    ///
    /// Stub — implementation will be completed in Task 3.
    pub fn setup_hooks(_pipe_name: &str) -> io::Result<()> {
        Ok(())
    }
    