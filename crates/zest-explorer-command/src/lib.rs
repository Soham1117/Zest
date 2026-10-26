//! In-proc COM server that adds "Convert with Zest" to the top-level
//! Windows 11 context menu (`IExplorerCommand`, registered by a sparse package).
#![allow(linker_messages)] // LNK4104: DllGetClassObject/DllCanUnloadNow are exported without a .def file.


pub mod args;

#[cfg(windows)]
mod com;
