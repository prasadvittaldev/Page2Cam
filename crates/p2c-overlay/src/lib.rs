//! Overlay layers: broadcast-style graphics (titles, tickers, HTML,
//! templates, pictures, videos, YouTube) drawn over the whole output by a
//! transparent second browser. This crate holds the platform-free parts: the
//! layer model and its checks ([`layer`]), the layer list with persistence
//! and auto-hide ([`set`]), stored media ([`assets`]), templates
//! ([`template`], built-ins embedded from `templates/`, user ones in
//! [`store`]), saved looks ([`scenes`]) and the scene page the overlay
//! browser shows ([`scene`]).
pub mod assets;
mod builtin;
pub mod layer;
pub mod qr;
pub mod scene;
pub mod scenes;
pub mod set;
pub mod store;
pub mod template;
pub mod util;

pub use assets::Assets;
pub use layer::{Anim, Content, Layer, ANIMS, KINDS, PRESETS};
pub use scenes::{Scene, SceneStore};
pub use set::{LayerSet, SceneLoad};
pub use store::TemplateLib;
pub use template::{builtin, builtins, Param, ParamType, Template, CATEGORIES};
