#![feature(iter_next_chunk)]
#![feature(slice_take)]
#![feature(iter_advance_by)]
#![feature(macro_metavar_expr)]
#![feature(associated_type_defaults)]
#![feature(impl_trait_in_assoc_type)]
#![feature(extract_if)]
#![feature(slice_from_ptr_range)]

mod extractors;
mod impls;

pub use {extractors::*, impls::*, rpc::CallId};
