//! The install pipeline (bottle pour): [`installer`] orchestrates resolve → download → extract →
//! relocate → receipt → link, using the `download`, `relocate`, `keg`, and `tab` subsystems.

pub mod extract;
pub mod installer;
