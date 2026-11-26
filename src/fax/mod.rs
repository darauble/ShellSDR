//! Weather fax (0.2.2, spec 15): the FAX library's C API (`ffi`, loaded
//! from `libfax.so.1`) and the receiver service of a FAX VFO (`service`).
//! The VFO type is `vfo::types::fax`, the view `ui::fax_view`.

pub mod ffi;
pub mod kitty;
pub mod service;
