//! Overlay layers: broadcast-style graphics (titles, tickers, HTML,
//! pictures, videos, YouTube) drawn over the whole output by a transparent
//! second browser. This crate holds the platform-free parts: the layer model
//! and its checks ([`layer`]), the layer list with persistence and
//! auto-hide ([`set`]), stored media ([`assets`]) and the scene page the
//! overlay browser shows ([`scene`]).
pub mod assets;
pub mod layer;
pub mod scene;
pub mod set;
pub mod util;

pub use assets::Assets;
pub use layer::{Anim, Content, Layer, ANIMS, KINDS, PRESETS};
pub use set::LayerSet;
